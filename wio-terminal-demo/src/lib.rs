#![no_std]

extern crate alloc;

pub mod weave;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;
use core::fmt;
use core::ops::ControlFlow;

use embedded_io::{ErrorType, Read, Seek, SeekFrom, Write};
use embedded_sdmmc::{
    Attributes, Block, BlockCount, BlockDevice, BlockIdx, ClusterId, DirEntry, FilenameError,
    LfnBuffer, Mode, RawDirectory, RawFile, RawVolume, ShortFileName, TimeSource, Timestamp,
    VolumeIdx, VolumeManager,
};

pub const MAX_FILE_STEM_CHARS: usize = 8;
/// Longest creatable entry name: an 8.3 short name (8 + dot + 3).
pub const MAX_ENTRY_NAME_CHARS: usize = 12;
pub const EXPLORER_PAGE_ROWS: usize = 20;

// ---------------------------------------------------------------------------
// Input

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Button {
    TopLeft,
    TopMiddle,
    TopRight,
    Up,
    Left,
    Click,
    Right,
    Down,
}

impl Button {
    pub const ALL: [Button; 8] = [
        Button::TopLeft,
        Button::TopMiddle,
        Button::TopRight,
        Button::Up,
        Button::Left,
        Button::Click,
        Button::Right,
        Button::Down,
    ];

    fn bit(self) -> u8 {
        1 << self as u8
    }

    fn repeats(self) -> bool {
        matches!(
            self,
            Button::Up | Button::Left | Button::Right | Button::Down
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawButtons(u8);

impl RawButtons {
    pub fn with(mut self, button: Button, pressed: bool) -> Self {
        if pressed {
            self.0 |= button.bit();
        } else {
            self.0 &= !button.bit();
        }
        self
    }

    pub fn pressed(self, button: Button) -> bool {
        self.0 & button.bit() != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonEvent {
    pub button: Button,
    pub repeat: bool,
}

/// Debounces buttons and adds joystick key-repeat. Times are RTC ticks at 1024 Hz.
pub struct InputEngine {
    observed: RawButtons,
    stable: RawButtons,
    changed_at: [u32; 8],
    pressed_at: [u32; 8],
    next_repeat: [u32; 8],
}

impl InputEngine {
    pub const DEBOUNCE_TICKS: u32 = 21;
    pub const FIRST_REPEAT_TICKS: u32 = 410;
    pub const REPEAT_TICKS: u32 = 102;

    pub const fn new() -> Self {
        Self {
            observed: RawButtons(0),
            stable: RawButtons(0),
            changed_at: [0; 8],
            pressed_at: [0; 8],
            next_repeat: [0; 8],
        }
    }

    /// Returns at most one event per poll, in physical-control priority order.
    pub fn update(&mut self, raw: RawButtons, now: u32) -> Option<Button> {
        self.update_event(raw, now, false).map(|event| event.button)
    }

    /// Returns a debounced press or held-control repeat. `repeat_top_sides`
    /// is enabled only while Universal Weave's reading panel is active.
    pub fn update_event(
        &mut self,
        raw: RawButtons,
        now: u32,
        repeat_top_sides: bool,
    ) -> Option<ButtonEvent> {
        for button in Button::ALL {
            let i = button as usize;
            if raw.pressed(button) != self.observed.pressed(button) {
                self.observed = self.observed.with(button, raw.pressed(button));
                self.changed_at[i] = now;
            }
            if self.observed.pressed(button) != self.stable.pressed(button)
                && now.wrapping_sub(self.changed_at[i]) >= Self::DEBOUNCE_TICKS
            {
                let down = self.observed.pressed(button);
                self.stable = self.stable.with(button, down);
                if down {
                    self.pressed_at[i] = now;
                    self.next_repeat[i] = now.wrapping_add(Self::FIRST_REPEAT_TICKS);
                    return Some(ButtonEvent {
                        button,
                        repeat: false,
                    });
                }
            }
        }

        for button in Button::ALL {
            let i = button as usize;
            // Requiring the raw `observed` state as well as the debounced
            // `stable` state stops repeats the moment a release is seen;
            // otherwise a deadline landing inside the release's debounce
            // window would emit one repeat after the user let go.
            if (button.repeats()
                || (repeat_top_sides && matches!(button, Button::TopLeft | Button::TopRight)))
                && self.stable.pressed(button)
                && self.observed.pressed(button)
                && now.wrapping_sub(self.next_repeat[i]) < u32::MAX / 2
            {
                self.next_repeat[i] = now.wrapping_add(Self::REPEAT_TICKS);
                return Some(ButtonEvent {
                    button,
                    repeat: true,
                });
            }
        }
        None
    }

    pub fn is_pressed(&self, button: Button) -> bool {
        self.stable.pressed(button)
    }

    pub fn held_ticks(&self, button: Button, now: u32) -> u32 {
        if self.is_pressed(button) {
            now.wrapping_sub(self.pressed_at[button as usize])
        } else {
            0
        }
    }
}

impl Default for InputEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Editor and on-screen keyboard

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardPage {
    Lower,
    Upper,
    Symbols,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Character(char),
    Case,
    Page,
    Space,
    Backspace,
    Delete,
    Enter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub page: KeyboardPage,
    pub row: usize,
    pub column: usize,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            page: KeyboardPage::Lower,
            row: 0,
            column: 0,
        }
    }

    pub fn selected(&self) -> Key {
        self.key_at(self.row, self.column)
    }

    pub fn row_len(&self, row: usize) -> usize {
        if row < 3 { 10 } else { 6 }
    }

    pub fn key_at(&self, row: usize, column: usize) -> Key {
        if row == 3 {
            return [
                Key::Case,
                Key::Page,
                Key::Space,
                Key::Backspace,
                Key::Delete,
                Key::Enter,
            ][column.min(5)];
        }
        let rows = match self.page {
            KeyboardPage::Lower => ["qwertyuiop", "asdfghjkl;", "zxcvbnm,./"],
            KeyboardPage::Upper => ["QWERTYUIOP", "ASDFGHJKL:", "ZXCVBNM?!\""],
            KeyboardPage::Symbols => ["1234567890", "-_=+[]{}()", "@#$%^&*'`~"],
        };
        Key::Character(rows[row.min(2)].chars().nth(column.min(9)).unwrap())
    }

    pub fn label(&self, row: usize, column: usize) -> String {
        match self.key_at(row, column) {
            Key::Character(ch) => ch.to_string(),
            Key::Case => "CASE".into(),
            Key::Page => "PAGE".into(),
            Key::Space => "SP".into(),
            Key::Backspace => "BS".into(),
            Key::Delete => "DEL".into(),
            Key::Enter => "ENT".into(),
        }
    }

    pub fn move_left(&mut self) {
        let len = self.row_len(self.row);
        self.column = (self.column + len - 1) % len;
    }

    pub fn move_right(&mut self) {
        self.column = (self.column + 1) % self.row_len(self.row);
    }

    pub fn move_up(&mut self) {
        self.row = (self.row + 3) % 4;
        self.column = self.column.min(self.row_len(self.row) - 1);
    }

    pub fn move_down(&mut self) {
        self.row = (self.row + 1) % 4;
        self.column = self.column.min(self.row_len(self.row) - 1);
    }

    pub fn activate_meta(&mut self, key: Key) {
        match key {
            Key::Case => {
                self.page = match self.page {
                    KeyboardPage::Lower => KeyboardPage::Upper,
                    KeyboardPage::Upper => KeyboardPage::Lower,
                    KeyboardPage::Symbols => KeyboardPage::Lower,
                }
            }
            Key::Page => {
                self.page = match self.page {
                    KeyboardPage::Lower => KeyboardPage::Upper,
                    KeyboardPage::Upper => KeyboardPage::Symbols,
                    KeyboardPage::Symbols => KeyboardPage::Lower,
                }
            }
            _ => {}
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

pub fn validate_file_stem(stem: &str) -> Result<(), &'static str> {
    if stem.is_empty() {
        return Err("Enter a file name");
    }
    if stem.contains('.') {
        return Err("Dots are only for the extension");
    }
    if stem.chars().count() > MAX_FILE_STEM_CHARS {
        return Err("Name is longer than 8 characters");
    }
    validate_entry_name(stem)
}

/// The stored filesystem can only create 8.3 short names, so
/// [`ShortFileName`] conversion is the authority on what is accepted.
pub fn validate_entry_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Enter a name");
    }
    // ShortFileName maps "." and ".." to the directory dot entries instead of
    // rejecting them; creating either would target the directory itself.
    if name == "." || name == ".." {
        return Err("Name is reserved");
    }
    if name.contains(' ') {
        return Err("Spaces do not fit in 8.3 names");
    }
    // ShortFileName::create_from_str silently strips a bare trailing dot
    // ("NOTES." becomes "NOTES"), so the stored name would differ from the
    // typed one and selection tracking would miss the entry afterwards.
    if name.ends_with('.') {
        return Err("Name cannot end with a dot");
    }
    let sfn = match ShortFileName::create_from_str(name) {
        Ok(sfn) => sfn,
        Err(FilenameError::FilenameEmpty) => return Err("Enter a name"),
        Err(FilenameError::NameTooLong | FilenameError::MisplacedPeriod) => {
            return Err("Use an 8.3 name like NOTES.TXT");
        }
        Err(_) => return Err("Name contains a FAT-reserved character"),
    };
    // create_from_str normalizes some inputs silently (repeated interior
    // dots: "A..TXT" is stored as "A.TXT"), so the stored name would differ
    // from the typed one and selection tracking would miss the entry. Round-
    // trip the parsed name; case aside (8.3 names store uppercase), anything
    // it does not reproduce is refused.
    if !sfn.to_string().eq_ignore_ascii_case(name) {
        return Err("Name would be stored differently");
    }
    Ok(())
}

pub fn is_uwe_file(name: &str) -> bool {
    name.get(name.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".uwe"))
}

pub fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        alloc::format!("/{name}")
    } else {
        alloc::format!("{parent}/{name}")
    }
}

pub fn parent_path(path: &str) -> String {
    if path == "/" {
        return "/".into();
    }
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(index) => path[..index].into(),
    }
}

// ---------------------------------------------------------------------------
// Raw SD byte stream and media probing

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdStreamError {
    DeviceRead(String),
    DeviceWrite(String),
    DeviceSize(String),
    OutOfBounds,
}

impl fmt::Display for SdStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceRead(detail) => write!(f, "SD card read error: {detail}"),
            Self::DeviceWrite(detail) => write!(f, "SD card write error: {detail}"),
            Self::DeviceSize(detail) => write!(f, "SD card size error: {detail}"),
            Self::OutOfBounds => f.write_str("SD access outside media bounds"),
        }
    }
}

impl core::error::Error for SdStreamError {}

impl embedded_io::Error for SdStreamError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            Self::DeviceRead(_) | Self::DeviceWrite(_) | Self::DeviceSize(_) => {
                embedded_io::ErrorKind::Other
            }
            Self::OutOfBounds => embedded_io::ErrorKind::InvalidInput,
        }
    }
}

pub struct SdStream<D: BlockDevice> {
    device: D,
    position: u64,
    length: u64,
}

impl<D: BlockDevice> SdStream<D> {
    pub fn new(device: D) -> Result<Self, SdStreamError> {
        let BlockCount(blocks) = device
            .num_blocks()
            .map_err(|error| SdStreamError::DeviceSize(alloc::format!("{error:?}")))?;
        Ok(Self {
            device,
            position: 0,
            length: u64::from(blocks) * 512,
        })
    }

    pub fn len(&self) -> u64 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn into_inner(self) -> D {
        self.device
    }
}

impl<D: BlockDevice> ErrorType for SdStream<D> {
    type Error = SdStreamError;
}

/// How many times a raw SD exchange is attempted before its failure is
/// surfaced. Single SPI commands flake on real cards, so the retry policy
/// lives in the `BlockDevice` implementation (the firmware's controller
/// device, which recovers the card between attempts): every raw exchange
/// gets this many chances exactly once. The library issues each device call
/// once and does not add its own retry layer (`delete_entry_checked`'s
/// cache-aware retries run under their own independent budget,
/// `DELETE_RETRY_ATTEMPTS`).
pub const SD_RETRY_ATTEMPTS: usize = 3;

/// The manager-level budget for `delete_entry_checked`'s cache-aware
/// retries. Deliberately a separate knob from [`SD_RETRY_ATTEMPTS`]: the
/// two policies serve different purposes (cache-echo disambiguation vs
/// transient-flake absorption), and each delete attempt already passes
/// through the device layer's full retry cycle, so tuning one must not
/// silently multiply the other.
const DELETE_RETRY_ATTEMPTS: usize = 3;

/// How many times a rollback's by-name resolution is re-asked while it
/// answers Unsure (see `CardFs::resolve_destination_name_retried`).
/// Mirrors [`DELETE_RETRY_ATTEMPTS`]'s tolerance for transient flakes:
/// without it, a single laundered read during either of the resolver's
/// two walks would refuse a cleanup that a retried delete used to
/// survive, silently stranding staging files.
const RESOLVE_RETRY_ATTEMPTS: usize = 3;

/// Run `operation` up to [`SD_RETRY_ATTEMPTS`] times, returning the first
/// success or the last attempt's error.
pub fn sd_retry<T, E>(mut operation: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    let mut result = operation();
    for _ in 1..SD_RETRY_ATTEMPTS {
        if result.is_ok() {
            break;
        }
        result = operation();
    }
    result
}

impl<D: BlockDevice> Read for SdStream<D> {
    fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let available = self.length.saturating_sub(self.position);
        let wanted = output
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let mut done = 0;
        while done < wanted {
            let sector_index = (self.position / 512) as u32;
            let sector_offset = (self.position % 512) as usize;
            let count = (wanted - done).min(512 - sector_offset);
            let mut block = Block::new();
            if let Err(error) = self
                .device
                .read(core::slice::from_mut(&mut block), BlockIdx(sector_index))
            {
                // Report the sectors already delivered; the next call starts
                // at the failing sector and surfaces the error with zero
                // progress, so Err never hides consumed bytes.
                return if done > 0 {
                    Ok(done)
                } else {
                    Err(SdStreamError::DeviceRead(alloc::format!("{error:?}")))
                };
            }
            output[done..done + count]
                .copy_from_slice(&block.contents[sector_offset..sector_offset + count]);
            self.position += count as u64;
            done += count;
        }
        Ok(done)
    }
}

impl<D: BlockDevice> Write for SdStream<D> {
    fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        if input.is_empty() {
            return Ok(0);
        }
        if self.position >= self.length {
            return Err(SdStreamError::OutOfBounds);
        }
        let wanted = input
            .len()
            .min(usize::try_from(self.length - self.position).unwrap_or(usize::MAX));
        let mut done = 0;
        while done < wanted {
            let sector_index = (self.position / 512) as u32;
            let sector_offset = (self.position % 512) as usize;
            let count = (wanted - done).min(512 - sector_offset);
            let mut block = Block::new();
            if sector_offset != 0 || count != 512 {
                if let Err(error) = self
                    .device
                    .read(core::slice::from_mut(&mut block), BlockIdx(sector_index))
                {
                    // As in read(): acknowledge the sectors already written
                    // so an Err always means nothing from this call reached
                    // the card.
                    return if done > 0 {
                        Ok(done)
                    } else {
                        Err(SdStreamError::DeviceRead(alloc::format!("{error:?}")))
                    };
                }
            }
            block.contents[sector_offset..sector_offset + count]
                .copy_from_slice(&input[done..done + count]);
            if let Err(error) = self
                .device
                .write(core::slice::from_ref(&block), BlockIdx(sector_index))
            {
                return if done > 0 {
                    Ok(done)
                } else {
                    Err(SdStreamError::DeviceWrite(alloc::format!("{error:?}")))
                };
            }
            self.position += count as u64;
            done += count;
        }
        Ok(done)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<D: BlockDevice> Seek for SdStream<D> {
    fn seek(&mut self, from: SeekFrom) -> Result<u64, Self::Error> {
        let next = match from {
            SeekFrom::Start(pos) => i128::from(pos),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => i128::from(self.length) + i128::from(delta),
        };
        if next < 0 || next > i128::from(self.length) {
            return Err(SdStreamError::OutOfBounds);
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaLayout {
    pub start_lba: u32,
    pub sector_count: u32,
    pub volume_serial: u32,
    /// Which MBR slot held the mountable partition. The probe skips damaged
    /// entries, so this is not always 0; the mount must open the same slot.
    /// Meaningless for superfloppy media (`start_lba == 0`).
    pub partition_index: u8,
    /// Whether the volume is FAT32 (at least 65,525 clusters). Gates the
    /// delete-time cluster reclaim, which is unsafe on FAT16.
    pub fat32: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeError<E> {
    Io(E),
    Unsupported,
    Invalid,
}

/// MBR status bits other than 0x80 (bootable) mark a slot embedded-sdmmc's
/// mount refuses; its `open_raw_volume` masks with 0x7F the same way. The
/// crate's check is private, so parity is proven by a host test instead of
/// a shared constant.
const MBR_STATUS_INVALID_MASK: u8 = 0x7f;

/// Partition types embedded-sdmmc 0.10 mounts (its `PARTITION_ID_*`
/// constants are private): FAT16-small, FAT16, FAT16-LBA, FAT32-CHS-LBA,
/// FAT32-LBA. Parity with the mount layer is proven by a host test.
const MOUNTABLE_PARTITION_TYPES: [u8; 5] = [0x04, 0x06, 0x0b, 0x0c, 0x0e];

/// Recognized-but-unmountable types (FAT12, extended, NTFS/exFAT,
/// extended-LBA, GPT protective): probe-side classification only, so a card
/// carrying one reads "unsupported" rather than "damaged".
const KNOWN_UNSUPPORTED_PARTITION_TYPES: [u8; 5] = [0x01, 0x05, 0x07, 0x0f, 0xee];

pub fn probe_fat<S: Read + Seek>(
    storage: &mut S,
    media_sectors: u32,
) -> Result<MediaLayout, ProbeError<S::Error>> {
    let mut sector = [0u8; 512];
    if !read_sector(storage, 0, &mut sector).map_err(ProbeError::Io)? {
        return Err(ProbeError::Invalid);
    }
    match parse_fat_boot_sector(&sector) {
        BootSector::Fat(geometry) => {
            if geometry.total_sectors > media_sectors {
                return Err(ProbeError::Invalid);
            }
            return Ok(MediaLayout {
                start_lba: 0,
                sector_count: geometry.total_sectors,
                volume_serial: geometry.serial,
                partition_index: 0,
                fat32: geometry.fat32,
            });
        }
        // exFAT, FAT12, and small-FAT32 boot sectors also end in 0x55AA, so
        // falling through would misread their boot code as an MBR and report
        // a healthy card as damaged instead of unsupported.
        BootSector::ExFat | BootSector::Fat12 | BootSector::SmallFat32 => {
            return Err(ProbeError::Unsupported);
        }
        BootSector::Invalid => {}
    }
    if sector[510..512] != [0x55, 0xaa] {
        return Err(ProbeError::Invalid);
    }
    let mut saw_unsupported = false;
    for index in 0..4 {
        let base = 446 + index * 16;
        // The mount layer refuses any slot whose status has a bit other than
        // 0x80 (bootable) set, so accepting one here would probe a partition
        // the mount can never open and dead-end a good card at the format
        // prompt instead of trying the remaining slots.
        if sector[base] & MBR_STATUS_INVALID_MASK != 0 {
            continue;
        }
        let kind = sector[base + 4];
        let start = le_u32(&sector[base + 8..base + 12]);
        let count = le_u32(&sector[base + 12..base + 16]);
        if KNOWN_UNSUPPORTED_PARTITION_TYPES.contains(&kind) {
            saw_unsupported = true;
            continue;
        }
        if !MOUNTABLE_PARTITION_TYPES.contains(&kind) || count == 0 {
            continue;
        }
        // A damaged or bogus entry disqualifies only itself: a later entry
        // may still hold a mountable partition (common after a partial
        // re-partitioning), so keep scanning instead of failing the probe.
        if start
            .checked_add(count)
            .is_none_or(|end| end > media_sectors)
        {
            continue;
        }
        let mut boot = [0u8; 512];
        if !read_sector(storage, start, &mut boot).map_err(ProbeError::Io)? {
            continue;
        }
        match parse_fat_boot_sector(&boot) {
            BootSector::Fat(geometry) => {
                if geometry.total_sectors > count {
                    continue;
                }
                return Ok(MediaLayout {
                    start_lba: start,
                    sector_count: geometry.total_sectors,
                    volume_serial: geometry.serial,
                    partition_index: index as u8,
                    fat32: geometry.fat32,
                });
            }
            BootSector::ExFat | BootSector::Fat12 | BootSector::SmallFat32 => {
                saw_unsupported = true
            }
            BootSector::Invalid => {}
        }
    }
    if saw_unsupported {
        Err(ProbeError::Unsupported)
    } else {
        Err(ProbeError::Invalid)
    }
}

/// Layout of a parsed FAT16/FAT32 volume, in sectors relative to its boot
/// sector: what the probe reports outward plus what the free-space
/// measurement needs to find the FAT and the FSInfo sector.
#[derive(Clone, Copy)]
struct VolumeGeometry {
    serial: u32,
    total_sectors: u32,
    reserved_sectors: u32,
    fat_sectors: u32,
    fat_copies: u8,
    sectors_per_cluster: u8,
    cluster_count: u32,
    /// FSInfo sector number from the FAT32 EBPB; meaningless on FAT16.
    fsinfo_sector: u16,
    fat32: bool,
}

enum BootSector {
    Fat(VolumeGeometry),
    Fat12,
    /// FAT32 EBPB layout with fewer than 65,525 clusters: the mount layer
    /// classifies FAT width by cluster count alone and would drive it as
    /// FAT16, misreading the volume and corrupting it on write.
    SmallFat32,
    ExFat,
    Invalid,
}

fn parse_fat_boot_sector(sector: &[u8; 512]) -> BootSector {
    if sector[510..512] != [0x55, 0xaa] {
        return BootSector::Invalid;
    }
    if &sector[3..11] == b"EXFAT   " {
        return BootSector::ExFat;
    }
    let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
    let sectors_per_cluster = sector[13];
    let reserved = u16::from_le_bytes([sector[14], sector[15]]) as u32;
    let fats = sector[16] as u32;
    let root_entries = u16::from_le_bytes([sector[17], sector[18]]) as u32;
    let total16 = u16::from_le_bytes([sector[19], sector[20]]) as u32;
    let total32 = le_u32(&sector[32..36]);
    let fat16 = u16::from_le_bytes([sector[22], sector[23]]) as u32;
    let fat32 = le_u32(&sector[36..40]);
    let total = if total16 != 0 { total16 } else { total32 };
    let fat_size = if fat16 != 0 { fat16 } else { fat32 };
    if bytes_per_sector != 512
        || sectors_per_cluster == 0
        || !sectors_per_cluster.is_power_of_two()
        || reserved == 0
        || fats == 0
        || total == 0
        || fat_size == 0
    {
        return BootSector::Invalid;
    }
    let root_sectors = (root_entries * 32).div_ceil(512);
    let overhead = match fats
        .checked_mul(fat_size)
        .and_then(|fat_total| reserved.checked_add(fat_total))
        .and_then(|sum| sum.checked_add(root_sectors))
    {
        Some(overhead) => overhead,
        None => return BootSector::Invalid,
    };
    let clusters = match total.checked_sub(overhead) {
        Some(data) => data / sectors_per_cluster as u32,
        None => return BootSector::Invalid,
    };
    if clusters < 4_085 {
        return BootSector::Fat12;
    }
    // The BPB layout decides everything past this point: root_entries == 0
    // and a zero 16-bit FAT size mean the FAT32 EBPB, where the serial lives
    // at offset 67 instead of 39.
    let fat32_layout = root_entries == 0 && fat16 == 0;
    // A declared FAT that cannot hold every cluster's entry is inconsistent
    // in a way no consumer may see: the manager's free-cluster allocator
    // maps clusters to FAT entry positions unchecked, so high clusters
    // would send reads and writes past the FAT into whatever follows it.
    // Rejecting the volume here routes the card to the format prompt and
    // lets mount, the free-space measurement, and the phantom-tail repair
    // trust plain arithmetic on any geometry this function returns.
    if fat32_layout {
        // FAT32 cluster numbers top out at 0x0fff_fff6 (higher values are
        // reserved marks).
        if clusters > 0x0fff_fff5
            || fat_entry_bytes(clusters, true).div_ceil(512) > u64::from(fat_size)
        {
            return BootSector::Invalid;
        }
    } else if clusters < 65_525
        && fat_entry_bytes(clusters, false).div_ceil(512) > u64::from(fat_size)
    {
        // Larger counts on the FAT16 layout fall to the Invalid arm below.
        return BootSector::Invalid;
    }
    if fat32_layout {
        if le_u32(&sector[44..48]) < 2 {
            return BootSector::Invalid;
        }
        // Out-of-spec FAT32 volumes with fewer than 65,525 clusters
        // (mkfs.vfat -F 32 on small media) must be refused: embedded-sdmmc
        // classifies FAT width by cluster count alone and would mount them
        // as FAT16, misreading every listing and corrupting the card on the
        // first write.
        if clusters < 65_525 {
            BootSector::SmallFat32
        } else {
            BootSector::Fat(VolumeGeometry {
                serial: le_u32(&sector[67..71]),
                total_sectors: total,
                reserved_sectors: reserved,
                fat_sectors: fat_size,
                fat_copies: fats as u8,
                sectors_per_cluster,
                cluster_count: clusters,
                fsinfo_sector: u16::from_le_bytes([sector[48], sector[49]]),
                fat32: true,
            })
        }
    } else if clusters < 65_525 {
        BootSector::Fat(VolumeGeometry {
            serial: le_u32(&sector[39..43]),
            total_sectors: total,
            reserved_sectors: reserved,
            fat_sectors: fat_size,
            fat_copies: fats as u8,
            sectors_per_cluster,
            cluster_count: clusters,
            fsinfo_sector: 0,
            fat32: false,
        })
    } else {
        BootSector::Invalid
    }
}

fn read_sector<S: Read + Seek>(
    storage: &mut S,
    lba: u32,
    sector: &mut [u8; 512],
) -> Result<bool, S::Error> {
    storage.seek(SeekFrom::Start(u64::from(lba) * 512))?;
    let mut done = 0;
    while done < sector.len() {
        let count = storage.read(&mut sector[done..])?;
        if count == 0 {
            return Ok(false);
        }
        done += count;
    }
    Ok(true)
}

/// Total bytes of FAT entries a volume with `cluster_count` data clusters
/// requires: entries 0 and 1 are reserved, so cluster N's entry ends at
/// (N + 3) * entry_size bytes into the FAT. Widened to u64 before the add
/// so no input can wrap. Shared by [`parse_fat_boot_sector`]'s
/// under-provision check, the formatter, [`repair_phantom_fat_tail`]'s
/// write bounds, and the tests that pin them, so the check that guarantees
/// "the declared FAT holds every entry" and the arithmetic that relies on
/// it can never diverge.
fn fat_entry_bytes(cluster_count: u32, fat32: bool) -> u64 {
    (u64::from(cluster_count) + 2) * if fat32 { 4 } else { 2 }
}

// ---------------------------------------------------------------------------
// FAT32 formatter

/// FAT "bad cluster" markers, shared by the formatter, the mount-time
/// phantom-tail repair, and the tests that pin both.
const FAT32_BAD_CLUSTER: u32 = 0x0fff_fff7;
const FAT16_BAD_CLUSTER: u16 = 0xfff7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatGeometry {
    pub partition_start: u32,
    pub partition_sectors: u32,
    pub sectors_per_cluster: u8,
    pub fat_sectors: u32,
    pub cluster_count: u32,
    pub volume_serial: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError<E> {
    Io(E),
    TooSmall,
    InvalidGeometry,
    VerifyFailed,
}

pub fn fat32_geometry(
    media_sectors: u32,
    serial_seed: u32,
) -> Result<FormatGeometry, FormatError<core::convert::Infallible>> {
    const PARTITION_START: u32 = 2_048;
    // The smallest partition that reaches FAT32's 65,525-cluster minimum:
    // 32 reserved sectors, two 512-sector FATs at one sector per cluster,
    // and the data sectors themselves. Anything below this would exhaust
    // every cluster-size candidate and report InvalidGeometry instead of
    // the truthful TooSmall.
    const MIN_PARTITION_SECTORS: u32 = 32 + 2 * 512 + 65_525;
    if media_sectors < PARTITION_START + MIN_PARTITION_SECTORS {
        return Err(FormatError::TooSmall);
    }
    let partition_sectors = media_sectors - PARTITION_START;
    let preferred: u32 = if partition_sectors <= 532_480 {
        1
    } else if partition_sectors <= 16_777_216 {
        8
    } else if partition_sectors <= 33_554_432 {
        16
    } else if partition_sectors <= 67_108_864 {
        32
    } else {
        64
    };
    let mut candidates = [64u8, 32, 16, 8, 4, 2, 1];
    candidates.sort_unstable_by_key(|spc| preferred.abs_diff(*spc as u32));
    for sectors_per_cluster in candidates {
        let spc = sectors_per_cluster as u32;
        let mut fat_sectors = 1u32;
        for _ in 0..32 {
            let overhead = 32u32.saturating_add(2u32.saturating_mul(fat_sectors));
            if overhead >= partition_sectors {
                break;
            }
            let clusters = (partition_sectors - overhead) / spc;
            let next = fat_entry_bytes(clusters, true).div_ceil(512) as u32;
            if next == fat_sectors {
                if (65_525..=0x0fff_fff5).contains(&clusters) {
                    return Ok(FormatGeometry {
                        partition_start: PARTITION_START,
                        partition_sectors,
                        sectors_per_cluster,
                        fat_sectors,
                        cluster_count: clusters,
                        // Mix the caller's seed so two same-size cards do not
                        // share a serial; the serial is what distinguishes a
                        // reinserted card from a different one.
                        volume_serial: 0x5749_4f00
                            ^ media_sectors
                            ^ serial_seed.wrapping_mul(0x9e37_79b9),
                    });
                }
                break;
            }
            fat_sectors = next;
        }
    }
    Err(FormatError::InvalidGeometry)
}

pub fn format_fat32<S: Read + Write + Seek>(
    storage: &mut S,
    media_sectors: u32,
    serial_seed: u32,
) -> Result<FormatGeometry, FormatError<S::Error>> {
    let geometry = fat32_geometry(media_sectors, serial_seed).map_err(|error| match error {
        FormatError::TooSmall => FormatError::TooSmall,
        _ => FormatError::InvalidGeometry,
    })?;
    let mut sector = [0u8; 512];

    // MBR and its single LBA-addressed FAT32 partition.
    sector[446 + 1..446 + 4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    sector[446 + 4] = 0x0c;
    sector[446 + 5..446 + 8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    put_u32(&mut sector[446 + 8..446 + 12], geometry.partition_start);
    put_u32(&mut sector[446 + 12..446 + 16], geometry.partition_sectors);
    sector[510..512].copy_from_slice(&[0x55, 0xaa]);
    write_sector(storage, 0, &sector).map_err(FormatError::Io)?;

    // A card that previously carried GPT keeps its primary header at LBA 1
    // and its backup header at the last LBA. Hosts that honor those remnants
    // over the fresh MBR would see a stale partition table, so erase both.
    let zero = [0u8; 512];
    write_sector(storage, 1, &zero).map_err(FormatError::Io)?;
    write_sector(storage, media_sectors - 1, &zero).map_err(FormatError::Io)?;

    let boot = make_boot_sector(&geometry);
    let info = make_fsinfo(&geometry);
    for offset in 0..32 {
        write_sector(storage, geometry.partition_start + offset, &zero).map_err(FormatError::Io)?;
    }
    write_sector(storage, geometry.partition_start, &boot).map_err(FormatError::Io)?;
    write_sector(storage, geometry.partition_start + 1, &info).map_err(FormatError::Io)?;
    write_sector(storage, geometry.partition_start + 6, &boot).map_err(FormatError::Io)?;
    write_sector(storage, geometry.partition_start + 7, &info).map_err(FormatError::Io)?;

    let first_fat = geometry.partition_start + 32;
    let phantom_offset = (fat_entry_bytes(geometry.cluster_count, true) % 512) as usize;
    for copy in 0..2 {
        let base = first_fat + copy * geometry.fat_sectors;
        for offset in 0..geometry.fat_sectors {
            write_sector(storage, base + offset, &zero).map_err(FormatError::Io)?;
        }
        let mut first = [0u8; 512];
        put_u32(&mut first[0..4], 0x0fff_fff8);
        put_u32(&mut first[4..8], 0xffff_ffff);
        put_u32(&mut first[8..12], 0x0fff_ffff);
        write_sector(storage, base, &first).map_err(FormatError::Io)?;
        // The FAT's last sector has spare entries beyond the volume's real
        // clusters. The manager's free-cluster search scans whole FAT
        // sectors without re-checking its end bound, so zeroed spares
        // would be handed out as allocatable clusters that map past the
        // end of the partition; mark them bad, as a real formatter does.
        // FAT32's 65,525-cluster minimum keeps fat_sectors >= 512, so this
        // is never the sector holding entries 0..2 above.
        if phantom_offset != 0 {
            let mut last = [0u8; 512];
            mark_phantom_tail(&mut last[phantom_offset..], FAT32_BAD_CLUSTER.to_le_bytes());
            write_sector(storage, base + geometry.fat_sectors - 1, &last)
                .map_err(FormatError::Io)?;
        }
    }

    let root = first_fat + 2 * geometry.fat_sectors;
    for offset in 0..u32::from(geometry.sectors_per_cluster) {
        write_sector(storage, root + offset, &zero).map_err(FormatError::Io)?;
    }
    storage.flush().map_err(FormatError::Io)?;

    let mut verify = [0u8; 512];
    if !read_sector(storage, 0, &mut verify).map_err(FormatError::Io)? {
        return Err(FormatError::VerifyFailed);
    }
    if verify[510..512] != [0x55, 0xaa] || le_u32(&verify[454..458]) != geometry.partition_start {
        return Err(FormatError::VerifyFailed);
    }
    if !read_sector(storage, geometry.partition_start, &mut verify).map_err(FormatError::Io)? {
        return Err(FormatError::VerifyFailed);
    }
    if verify[510..512] != [0x55, 0xaa] || &verify[82..90] != b"FAT32   " {
        return Err(FormatError::VerifyFailed);
    }
    if !read_sector(storage, geometry.partition_start + 1, &mut verify).map_err(FormatError::Io)? {
        return Err(FormatError::VerifyFailed);
    }
    if le_u32(&verify[0..4]) != 0x4161_5252 || le_u32(&verify[484..488]) != 0x6141_7272 {
        return Err(FormatError::VerifyFailed);
    }
    Ok(geometry)
}

fn make_boot_sector(g: &FormatGeometry) -> [u8; 512] {
    let mut b = [0u8; 512];
    b[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"WIOEDIT ");
    put_u16(&mut b[11..13], 512);
    b[13] = g.sectors_per_cluster;
    put_u16(&mut b[14..16], 32);
    b[16] = 2;
    b[21] = 0xf8;
    put_u32(&mut b[28..32], g.partition_start);
    put_u16(&mut b[24..26], 63);
    put_u16(&mut b[26..28], 255);
    put_u32(&mut b[32..36], g.partition_sectors);
    put_u32(&mut b[36..40], g.fat_sectors);
    put_u32(&mut b[44..48], 2);
    put_u16(&mut b[48..50], 1);
    put_u16(&mut b[50..52], 6);
    b[64] = 0x80;
    b[66] = 0x29;
    put_u32(&mut b[67..71], g.volume_serial);
    b[71..82].copy_from_slice(b"WIO-TERM   ");
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510..512].copy_from_slice(&[0x55, 0xaa]);
    b
}

fn make_fsinfo(g: &FormatGeometry) -> [u8; 512] {
    let mut b = [0u8; 512];
    put_u32(&mut b[0..4], 0x4161_5252);
    put_u32(&mut b[484..488], 0x6141_7272);
    put_u32(&mut b[488..492], g.cluster_count.saturating_sub(1));
    put_u32(&mut b[492..496], 3);
    put_u32(&mut b[508..512], 0xaa55_0000);
    b
}

fn write_sector<S: Write + Seek>(
    storage: &mut S,
    lba: u32,
    sector: &[u8; 512],
) -> Result<(), S::Error> {
    storage.seek(SeekFrom::Start(u64::from(lba) * 512))?;
    storage.write_all(sector)
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte field"))
}

fn put_u16(bytes: &mut [u8], value: u16) {
    bytes.copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], value: u32) {
    bytes.copy_from_slice(&value.to_le_bytes());
}

// ---------------------------------------------------------------------------
// Filesystem helpers

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub hidden: bool,
    pub system: bool,
}

/// The device has no calendar clock, so entries written on it carry a fixed
/// timestamp instead of values invented from the 1024 Hz RTC tick counter.
pub struct FixedTimeSource;

impl TimeSource for FixedTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 50,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

#[derive(Debug)]
pub enum FsOpError<E: core::error::Error> {
    Fs(embedded_sdmmc::Error<E>),
    AlreadyExists,
    IsADirectory,
    NotADirectory,
    MoveIntoSelf,
    VerifyFailed,
    /// A subtree operation met nesting deeper than [`MAX_TREE_DEPTH`].
    TooDeep,
    /// A DiskFull report from the manager while the card no longer answers
    /// even a probe read. The manager masks device faults met while
    /// extending a file's cluster chain as DiskFull; reporting that as a
    /// full card would invite the user to delete files on a dead card.
    DeviceUnresponsive,
    /// A delete failed after destruction persisted -- or, when the fault
    /// struck mid-truncate, may have persisted: a file's contents reclaimed
    /// (cluster chain freed, size zeroed by the pre-delete reclaim, leaving
    /// the entry listed but empty) or some of a
    /// folder's children removed before the failure. Distinct from `Fs` so
    /// callers can report the true state instead of implying the data is
    /// intact -- after a verified move this means "data safe at the
    /// destination, source left as an empty ghost". Wraps the underlying
    /// failure.
    DeleteIncomplete(Box<FsOpError<E>>),
    /// A copy/move failed AND removing the partial destination failed too
    /// -- or was refused as unsafe: when duplicate-name ambiguity means a
    /// by-name delete could resolve to a pre-existing entry instead of
    /// this operation's debris, the debris is reported rather than
    /// gambled on. Either way something bearing the destination name
    /// remains, and a retry will stop at "already exists" until it is
    /// deleted. Wraps the original failure.
    CleanupIncomplete(Box<FsOpError<E>>),
    /// A move's copy fully verified at the destination, but removing the
    /// source failed with its contents (possibly) intact -- the
    /// no-destruction counterpart of `DeleteIncomplete`. Success with a
    /// caveat: the destination is the
    /// operation of record and must never be rolled back (it may hold the
    /// only good copy); the stale source entry remains and a retried move
    /// will stop at "already exists" until it is removed. Wraps the
    /// delete's failure.
    SourceRemains(Box<FsOpError<E>>),
    /// A same-name collision met while copying children *after* a move's
    /// destination root landed, with the partial destination verified
    /// unique and fully removed. Distinct from `AlreadyExists`, which
    /// promises a pre-existing entry blocks the operation: here nothing
    /// bearing the destination name remains (the collision came from the
    /// source side -- duplicate 8.3 entries written by another machine,
    /// or a listing shifted by a read fault).
    CopyCollision,
}

/// A failed transactional save, plus whether a staging backup provably
/// survived: `backup_kept` names the on-card `~WIO*.BAK` (full path) that
/// still holds the original contents when the target may not.
#[derive(Debug)]
pub struct SaveError<E: core::error::Error> {
    pub error: FsOpError<E>,
    pub backup_kept: Option<String>,
}

impl<E: core::error::Error> From<FsOpError<E>> for SaveError<E> {
    fn from(error: FsOpError<E>) -> Self {
        Self {
            error,
            backup_kept: None,
        }
    }
}

impl<E: core::error::Error> From<embedded_sdmmc::Error<E>> for FsOpError<E> {
    fn from(error: embedded_sdmmc::Error<E>) -> Self {
        match error {
            embedded_sdmmc::Error::FileAlreadyExists | embedded_sdmmc::Error::DirAlreadyExists => {
                Self::AlreadyExists
            }
            other => Self::Fs(other),
        }
    }
}

impl<E: core::error::Error> FsOpError<E> {
    /// Whether this is a proven device I/O fault, after which the
    /// filesystem's metadata cache no longer proves what reached the card.
    /// Production code gates rollback via `assess_failure` (which also
    /// covers the manager's masked forms); this narrower check remains for
    /// the host tests' assertions.
    pub fn is_device_error(&self) -> bool {
        matches!(self, Self::Fs(embedded_sdmmc::Error::DeviceError(_)))
    }

    fn is_not_found(&self) -> bool {
        matches!(self, Self::Fs(embedded_sdmmc::Error::NotFound))
    }
}

/// Copy buffer for rename/move emulation; moved files can be far larger than
/// file size, so they stream through this fixed window.
const COPY_CHUNK_BYTES: usize = 4096;
/// Children fetched per directory pass; bounds heap use when recursing into
/// folder trees written by other machines, which can hold thousands of
/// entries per directory.
const CHILD_BATCH: usize = 32;
/// Nesting cap for subtree walks (recursive delete, move, and their
/// pre-scans); bounds stack use and the one-batch-per-level memory that
/// stays live while descending.
const MAX_TREE_DEPTH: usize = 32;
/// Cap on how much [`CardFs::file_reads_back_intact`] reads back to prove
/// a file unharmed. Larger files keep the conservative
/// destruction-possible verdict instead of stalling the single-threaded
/// UI on a full-file SPI readback.
const INTACT_PROBE_MAX_BYTES: usize = 64 * 1024;

/// One child of a directory, copied out of the manager's iteration callback
/// so callers can act on it after every handle is closed.
struct ChildEntry {
    name: String,
    is_dir: bool,
    read_only: bool,
    /// Interrupted-make_dir debris whose cluster-0 entry embedded-sdmmc
    /// remaps to the volume root (see [`entry_is_root_alias_debris`]):
    /// walking it walks the real root, so recursions must never descend
    /// into it.
    aliases_root: bool,
}

/// The verdict of two cache-dropped, agreeing walks of a name's parent
/// directory (see [`CardFs::resolve_destination_name`]). The walks decide
/// what a rollback's by-name mutation may act on, so a single launder-prone
/// listing is never enough.
enum NameResolution {
    /// Both walks saw zero matches: confirmed absence. (Two laundered
    /// walks in a row -- a persistent outage blinding both -- is the
    /// accepted residue, exactly as for [`CardFs::confirmed_lookup`].)
    Absent,
    /// Both walks saw exactly this one match, at the same on-disk slot.
    Unique(DirEntry),
    /// More than one match, a walk error, or disagreement between the
    /// walks: no by-name mutation may act.
    Unsure,
}

/// Whether a rollback cleanup left the destination name clean. A refusal
/// (unsafe to act on the name) and a failed delete fold together: every
/// caller reports both as [`FsOpError::CleanupIncomplete`], the
/// retry-blocking debris being the actionable fact either way.
#[derive(PartialEq)]
enum CleanupOutcome {
    Clean,
    Blocked,
}
/// 255 UTF-16 units re-encode to at most 765 UTF-8 bytes.
const LFN_BUFFER_BYTES: usize = 768;

/// Path-based facade over [`VolumeManager`]. Paths are `/`-separated strings
/// whose components are 8.3 short names; long filenames of existing entries
/// surface only as display names in [`DirectoryItem::name`]. Every operation
/// opens the handles it needs and closes them before returning (at most two
/// directories and two files at once), so the manager's default handle limits
/// are never approached and no state needs flushing on unmount: dropping the
/// [`CardFs`] is enough.
pub struct CardFs<D: BlockDevice> {
    mgr: VolumeManager<D, FixedTimeSource>,
    /// Replaced in place when the volume is cycled to flush its metadata;
    /// see [`Self::flush_volume_metadata`].
    volume: Cell<RawVolume>,
    layout: MediaLayout,
    /// Cached [`Self::free_space_bytes`] result, so browsing never re-reads
    /// the card for a figure that cannot have changed.
    free_space: Cell<Option<u64>>,
    /// Where the cached figure (and, on FAT32, the on-disk FSInfo sector
    /// and the volume handle behind it) stands; see [`FreeSpaceState`].
    free_space_state: Cell<FreeSpaceState>,
}

/// Lifecycle of the free-space cache and the FAT32 metadata flush that
/// feeds it. One state cell instead of coupled flags, so the invalid
/// combinations are unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreeSpaceState {
    /// The cached figure (if any) still matches the card.
    Clean,
    /// A mutation ran: the figure (and, on FAT32, the on-disk FSInfo
    /// sector) may no longer match the FAT; re-flush before measuring.
    Stale,
    /// The FAT32 metadata flush failed with the mounted volume intact
    /// (the close was refused). Latched so a hung-but-seated card does
    /// not pay the close/reopen cycle on every explorer refresh; cleared
    /// by the next mutation, which proves someone reached the card again.
    FlushFailed,
    /// `close_volume` deregistered the volume but the reopen failed: the
    /// stored [`RawVolume`] handle is dead and every operation reports a
    /// bad handle until it is repaired. Each free-space query retries
    /// only the reopen (cheap on a recovered card), so browsing
    /// self-heals without waiting for a mutation or a card reseat.
    VolumeDead,
}

impl<D: BlockDevice> CardFs<D> {
    /// Mount the FAT volume described by `layout` (from [`probe_fat`]).
    pub fn mount(device: D, layout: MediaLayout) -> Result<Self, embedded_sdmmc::Error<D::Error>> {
        repair_phantom_fat_tail(&device, &layout);
        let mgr = VolumeManager::new(device, FixedTimeSource);
        let volume = mgr.open_raw_volume(VolumeIdx(usize::from(layout.partition_index)))?;
        Ok(Self {
            mgr,
            volume: Cell::new(volume),
            layout,
            free_space: Cell::new(None),
            free_space_state: Cell::new(FreeSpaceState::Clean),
        })
    }

    /// Every mutating operation calls this: the cached free-space figure
    /// (and, on FAT32, the on-disk FSInfo sector) may no longer match the
    /// FAT. It also re-arms a failed metadata flush -- the mutation proves
    /// someone is talking to the card again, so the next free-space query
    /// may retry the volume cycle once, instead of every explorer refresh
    /// repeating a multi-second reopen ordeal on a hung card. A dead
    /// volume handle is the exception: a mutation cannot repair it (it
    /// fails on the bad handle itself), so the state must keep demanding
    /// the reopen retry instead of being downgraded to plain staleness.
    fn mark_free_space_stale(&self) {
        if self.free_space_state.get() != FreeSpaceState::VolumeDead {
            self.free_space_state.set(FreeSpaceState::Stale);
        }
    }

    /// Bytes still free on the volume, or `None` when the figure cannot be
    /// determined. FAT32 volumes report the FSInfo sector's free-cluster
    /// count (a card formatted elsewhere may carry none); FAT16 has no
    /// FSInfo sector, but its FAT is at most 128 KiB, so the free entries
    /// are counted directly. Errors are absorbed into `None`: the figure is
    /// advisory. A failed FAT32 metadata flush latches (see
    /// [`FreeSpaceState`]): with the volume intact, later queries keep
    /// answering `None` without touching the card until the next mutation;
    /// with the volume handle dead, each query retries only the reopen so
    /// browsing self-heals as soon as the card answers again. Other misses
    /// simply retry on the next query.
    ///
    /// The FAT32 figure can run low: the manager's truncate frees a chain's
    /// final cluster in the FAT without counting it, so every overwrite or
    /// delete of a multi-cluster file leaves the count one cluster short.
    /// The drift is conservative (space is never overstated) and a PC disk
    /// check rebuilds the FSInfo count from the FAT.
    pub fn free_space_bytes(&self) -> Option<u64> {
        match self.free_space_state.get() {
            // A previous flush already failed with the volume intact and
            // nothing has touched the card since; retrying now would just
            // freeze the UI again.
            FreeSpaceState::FlushFailed => return None,
            FreeSpaceState::VolumeDead => {
                if self.reopen_volume().is_err() {
                    return None;
                }
                // No re-flush: nothing mutated while the handle was dead
                // (mutations fail on the bad handle, and
                // mark_free_space_stale refuses to downgrade VolumeDead),
                // and the reopen loaded the on-disk FSInfo into the manager
                // -- a flush would write back exactly what was just read,
                // buying nothing but a second chance to kill the fresh
                // handle. Only the cached figure is suspect: it predates
                // the failed flush.
                self.free_space_state.set(FreeSpaceState::Clean);
                self.free_space.set(None);
            }
            FreeSpaceState::Clean | FreeSpaceState::Stale => {}
        }
        if self.free_space_state.get() == FreeSpaceState::Stale {
            if self.layout.fat32 {
                if let Err(state) = self.flush_volume_metadata() {
                    self.free_space_state.set(state);
                    return None;
                }
            }
            self.free_space_state.set(FreeSpaceState::Clean);
            self.free_space.set(None);
        }
        if let Some(bytes) = self.free_space.get() {
            return Some(bytes);
        }
        let bytes = self.measure_free_space();
        self.free_space.set(bytes);
        bytes
    }

    /// The manager keeps its running FAT32 free-cluster count in memory and
    /// only writes it back to the FSInfo sector when a dirty file flushes or
    /// the volume closes; the delete-time cluster reclaim is a truncate that
    /// never dirties a file, so it leaves the on-disk copy behind. Cycling
    /// the volume forces that write so [`Self::measure_free_space`] can read
    /// the sector as the truth.
    fn flush_volume_metadata(&self) -> Result<(), FreeSpaceState> {
        match self.mgr.close_volume(self.volume.get()) {
            // The volume was not closed. Every operation closes its handles
            // before returning, so this cannot happen, but the mounted
            // volume is still valid and must not be reopened over.
            Err(embedded_sdmmc::Error::VolumeStillInUse) => Err(FreeSpaceState::FlushFailed),
            // Every other outcome (including a failed FSInfo write) removed
            // the volume from the manager, so a reopen is required either
            // way. A failed reopen leaves the stored handle dead and every
            // later operation reporting a bad handle until it is repaired:
            // the caller latches `VolumeDead` and each later free-space
            // query retries only the reopen until one succeeds.
            _ => self.reopen_volume().map_err(|_| FreeSpaceState::VolumeDead),
        }
    }

    /// Re-open the mounted partition and store the fresh handle. The single
    /// volume-handle repair sequence shared by the flush path and the
    /// `VolumeDead` self-heal.
    fn reopen_volume(&self) -> Result<(), embedded_sdmmc::Error<D::Error>> {
        let volume = self
            .mgr
            .open_raw_volume(VolumeIdx(usize::from(self.layout.partition_index)))?;
        self.volume.set(volume);
        Ok(())
    }

    /// One read of the volume's boot sector: cheap, read-only proof
    /// the card still answers before rollback mutations are attempted. This
    /// only proves reads work; a card that fails only writes passes, but
    /// then every rollback write fails and is swallowed without persisting
    /// anything destructive.
    fn device_is_responding(&self) -> bool {
        self.mgr
            .device(|device| read_device_sector(device, self.layout.start_lba))
            .is_some()
    }

    /// Decide whether cleanup/rollback mutations may follow a failed
    /// mutating step, re-tagging an error the manager is known to mask.
    ///
    /// `VolumeManager::write` reports a device fault met while extending a
    /// file as `DiskFull`, the directory machinery reports one met during
    /// its free-slot chain walk as `NotEnoughSpace`, and a fault met right
    /// after a successful extension surfaces as `AllocationError`, so none
    /// can be taken at face value: `AllocationError` has no non-fault
    /// producer and is treated as a device fault outright; the two
    /// full-card errors are usually genuine (and rollback is then exactly
    /// what is wanted), so the card is probed first, and a failed probe
    /// re-tags the error as [`FsOpError::DeviceUnresponsive`].
    ///
    /// Returns the (possibly re-tagged) error and whether rollback
    /// mutations should follow: refused both when further mutations are
    /// unsafe (device faults, destruction already under way) and when there
    /// is provably nothing to roll back (`AlreadyExists`).
    fn assess_failure(&self, error: FsOpError<D::Error>) -> (FsOpError<D::Error>, bool) {
        use embedded_sdmmc::Error;
        if matches!(
            error,
            FsOpError::Fs(Error::DeviceError(_) | Error::AllocationError)
                | FsOpError::DeviceUnresponsive
                | FsOpError::DeleteIncomplete(_)
                | FsOpError::CleanupIncomplete(_)
                | FsOpError::SourceRemains(_)
                // AlreadyExists proves the failed step created nothing: the
                // blocking entry pre-existed (every creating open is
                // preceded by a confirmed-absence check, so reaching it at
                // all means that check was double-laundered). Rollback
                // would delete a pre-existing entry, never this
                // operation's debris. The one step that outlives this
                // reasoning -- a child collision met after a move's
                // destination root landed -- is carved out by
                // `cleanup_partial_destination`'s `rollback_already_exists`
                // parameter.
                | FsOpError::AlreadyExists
                // Cleanup already ran (and completed) by construction.
                | FsOpError::CopyCollision
        ) {
            return (error, false);
        }
        if matches!(
            error,
            FsOpError::Fs(Error::DiskFull | Error::NotEnoughSpace)
        ) && !self.device_is_responding()
        {
            return (FsOpError::DeviceUnresponsive, false);
        }
        (error, true)
    }

    fn measure_free_space(&self) -> Option<u64> {
        let start = self.layout.start_lba;
        self.mgr.device(|device| {
            let boot = read_device_sector(device, start)?;
            let BootSector::Fat(geometry) = parse_fat_boot_sector(&boot) else {
                return None;
            };
            let free_clusters = if geometry.fat32 {
                // The mount already validated the FSInfo sector this BPB
                // pointer names (a bad pointer or signature refuses the
                // whole volume), so re-checking the signatures here only
                // guards against the sector changing under us.
                let info = read_device_sector(device, start + u32::from(geometry.fsinfo_sector))?;
                if le_u32(&info[0..4]) != 0x4161_5252 || le_u32(&info[484..488]) != 0x6141_7272 {
                    return None;
                }
                le_u32(&info[488..492])
            } else {
                let mut free = 0u32;
                let mut cluster = 2u32; // FAT entries 0 and 1 are reserved
                let end = geometry.cluster_count + 2;
                for index in 0..geometry.fat_sectors {
                    if cluster >= end {
                        break;
                    }
                    let sector =
                        read_device_sector(device, start + geometry.reserved_sectors + index)?;
                    let mut offset = (cluster as usize * 2) % 512;
                    while offset < 512 && cluster < end {
                        if sector[offset..offset + 2] == [0, 0] {
                            free += 1;
                        }
                        cluster += 1;
                        offset += 2;
                    }
                }
                free
            };
            // 0xFFFF_FFFF marks "unknown" in FSInfo, and any count beyond
            // the cluster total is equally meaningless.
            (free_clusters <= geometry.cluster_count)
                .then(|| u64::from(free_clusters) * u64::from(geometry.sectors_per_cluster) * 512)
        })
    }

    /// Walk `path` from the root, holding at most two directories open.
    fn open_dir_path(&self, path: &str) -> Result<RawDirectory, FsOpError<D::Error>> {
        let mut dir = self.mgr.open_root_dir(self.volume.get())?;
        for component in components(path) {
            // Interrupted-make_dir debris (a directory entry with first
            // cluster 0) opens as the volume root, so traversing through
            // it would silently redirect every deeper lookup -- listing,
            // save, move, delete -- onto the real root. Refuse on a
            // positive sighting; NotFound or a read error falls through
            // to the open below, which confirms absence itself.
            if let Ok(entry) = self.mgr.find_directory_entry(dir, component) {
                if entry_is_root_alias_debris(&entry) {
                    let _ = self.mgr.close_dir(dir);
                    return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster));
                }
            }
            // The manager launders FAT-chain read errors into NotFound
            // (see [`Self::find_entry`]); confirm a missing path component
            // before failing the whole walk, so every path-based operation
            // shares the same two-laundered-reads residue as the leaf
            // lookups.
            let next = self.confirmed_lookup(|| self.mgr.open_dir(dir, component));
            match next {
                Ok(next) => {
                    let _ = self.mgr.close_dir(dir);
                    dir = next;
                }
                Err(error) => {
                    let _ = self.mgr.close_dir(dir);
                    return Err(error.into());
                }
            }
        }
        Ok(dir)
    }

    /// Run `operation` with `path` opened as a directory, closing the handle
    /// on every path. All handle discipline lives here so operations can use
    /// `?` freely.
    fn with_dir<R>(
        &self,
        path: &str,
        operation: impl FnOnce(RawDirectory) -> Result<R, FsOpError<D::Error>>,
    ) -> Result<R, FsOpError<D::Error>> {
        let dir = self.open_dir_path(path)?;
        let result = operation(dir);
        let _ = self.mgr.close_dir(dir);
        result
    }

    /// Open the file at `path`, holding no directory handle afterwards: the
    /// file handle stays valid after its parent directory is closed.
    fn open_file_at(&self, path: &str, mode: Mode) -> Result<RawFile, FsOpError<D::Error>> {
        self.with_dir(&parent_path(path), |dir| {
            // The open's internal lookup launders FAT-chain read errors
            // into NotFound like every manager directory walk (see
            // [`Self::find_entry`]); confirm before reporting a missing
            // file.
            self.confirmed_lookup(|| self.mgr.open_file_in_dir(dir, leaf_name(path), mode))
                .map_err(FsOpError::from)
        })
    }

    /// Look up `path`, accepting a `NotFound` answer only after a second,
    /// cache-dropped ask of the same open directory agrees. The manager
    /// launders FAT-chain read errors into `NotFound` (see
    /// [`Self::entry_confirmed_absent`]), and several callers act
    /// destructively on absence -- skipping a save's backup staging, reusing
    /// a staging name, trusting that a move's destination is free -- while
    /// others treat absence as proof an operation landed. Confirming here
    /// covers every caller; a wrong verdict now needs two laundered reads in
    /// a row: the accepted residue.
    fn find_entry(&self, path: &str) -> Result<DirEntry, FsOpError<D::Error>> {
        self.with_dir(&parent_path(path), |dir| {
            self.confirmed_lookup(|| self.mgr.find_directory_entry(dir, leaf_name(path)))
                .map_err(FsOpError::from)
        })
    }

    /// Ask `lookup` once and, on `NotFound`, drop the block cache and ask
    /// again: the manager launders FAT-chain read errors into `NotFound`
    /// (see [`Self::find_entry`]), so a single answer never proves absence.
    fn confirmed_lookup<T>(
        &self,
        mut lookup: impl FnMut() -> Result<T, embedded_sdmmc::Error<D::Error>>,
    ) -> Result<T, embedded_sdmmc::Error<D::Error>> {
        match lookup() {
            Err(embedded_sdmmc::Error::NotFound) => {
                self.drop_block_cache();
                lookup()
            }
            other => other,
        }
    }

    /// Invalidate the manager's one-block cache (`device()` drops it as a
    /// side effect of handing out the device), so the next read fetches
    /// true on-disk state instead of a failed attempt's echo or a stale
    /// block.
    fn drop_block_cache(&self) {
        self.mgr.device(|_| ());
    }

    fn delete_file_at(&self, path: &str) -> Result<(), FsOpError<D::Error>> {
        self.with_dir(&parent_path(path), |dir| {
            self.delete_file_in_dir(dir, leaf_name(path))
        })
    }

    /// The truncate-then-checked-delete pairing every file removal shares.
    /// The result encodes destruction: `Ok` or `DeleteIncomplete` means the
    /// contents (and possibly the entry) are gone -- or, when the fault
    /// struck at the truncate rather than after it, possibly gone (see
    /// [`Self::reclaim_file_clusters_in_dir`]); every other error refused
    /// before destroying anything.
    fn delete_file_in_dir(&self, dir: RawDirectory, name: &str) -> Result<(), FsOpError<D::Error>> {
        let truncated = self.reclaim_file_clusters_in_dir(dir, name);
        self.delete_entry_checked(dir, name, truncated)
    }

    /// Best-effort, FAT32-only: free a file's cluster chain (all but the
    /// anchor cluster, which the manager's truncate keeps) before its entry
    /// is deleted, because `delete_entry_in_dir` never touches the FAT and
    /// the orphaned chain would otherwise consume space until a PC disk
    /// check. Errors are swallowed -- the delete that follows is the
    /// operation of record, and a directory or already-open entry simply
    /// stays unreclaimed. Gated off FAT16 because the manager accepts a
    /// corrupt FAT16 chain stepping onto entry 0x0000 as valid, and a
    /// truncate would then clobber FAT entry 0.
    ///
    /// Returns whether the destructive truncate persisted (or, on a device
    /// error mid-truncate, may have): the contents are gone even though the
    /// entry remains. The device-error arm can also fire before anything
    /// was touched at all -- the open's directory walk and the truncate's
    /// opening FAT chain read both precede the first FAT write -- so it
    /// probes the file back: contents that still read through completely
    /// prove an unbroken chain, i.e. nothing was destroyed, and every
    /// `DeleteIncomplete` consumer then stops reporting destruction that
    /// never happened. A probe that cannot prove intactness keeps the
    /// destruction-possible verdict; a caller that must further
    /// distinguish content re-proves it by readback, as
    /// [`Self::save_transactional`]'s rollback does before naming a kept
    /// backup.
    fn reclaim_file_clusters_in_dir(&self, dir: RawDirectory, name: &str) -> bool {
        if !self.layout.fat32 {
            return false;
        }
        match self
            .mgr
            .open_file_in_dir(dir, name, Mode::ReadWriteTruncate)
        {
            Ok(file) => {
                let _ = self.mgr.close_file(file);
                true
            }
            // A device error can strike mid-truncate, after FAT sectors were
            // already rewritten; destruction is possible unless the readback
            // proves otherwise. Every other error (NotFound,
            // FileAlreadyOpen, OpenedDirAsFile...) means the open refused
            // before touching anything.
            Err(embedded_sdmmc::Error::DeviceError(_)) => !self.file_reads_back_intact(dir, name),
            Err(_) => false,
        }
    }

    /// After a truncate met a device error: whether the contents are
    /// provably still whole. Every one of the entry's `size` bytes must
    /// read back through the FAT chain (a partially freed chain stops the
    /// read short or errors), through a fixed stack chunk so the probe
    /// never buffers the file. Any lookup, open, or read failure keeps the
    /// destruction-possible verdict -- this only ever *downgrades* a
    /// `DeleteIncomplete` report, so it must prove, not guess.
    ///
    /// A zero on-disk size proves nothing: the size read here post-dates
    /// the truncate attempt, whose size-zeroing entry write can persist
    /// despite the reported error (the same late-status fault
    /// [`Self::delete_entry_checked`] models), and a destroyed file would
    /// then read back 0 == 0 "intact". An originally empty file loses no
    /// contents to a truncate, so its destruction-possible verdict is
    /// harmless. The readback is also capped: deleted files are not
    /// may be arbitrarily large, and streaming a foreign multi-hundred-MB
    /// file through SPI to soften error wording would
    /// freeze the single-threaded UI for minutes.
    fn file_reads_back_intact(&self, dir: RawDirectory, name: &str) -> bool {
        // The failed truncate may have left its phantom-modified block in
        // the manager's one-block cache; read true on-disk state.
        self.drop_block_cache();
        let Ok(entry) = self.mgr.find_directory_entry(dir, name) else {
            return false;
        };
        if entry.size == 0 || entry.size as usize > INTACT_PROBE_MAX_BYTES {
            return false;
        }
        let Ok(file) = self.mgr.open_file_in_dir(dir, name, Mode::ReadOnly) else {
            return false;
        };
        let mut chunk = [0u8; 512];
        let mut total = 0usize;
        let intact = loop {
            match self.read_full(file, &mut chunk) {
                Ok(0) => break total == entry.size as usize,
                Ok(count) => total += count,
                Err(_) => break false,
            }
        };
        let _ = self.mgr.close_file(file);
        intact
    }

    /// Whether a fresh lookup confirms `name` is absent from `dir`. The
    /// manager launders FAT-chain read errors into `NotFound` (its
    /// directory walks discard `next_cluster` failures and fall through to
    /// "no such entry"), so a `NotFound` from a delete is never taken at
    /// face value: absence must be confirmed by a second, independent walk.
    /// That walk can launder too, so a wrong verdict now needs two
    /// laundered reads in a row -- the accepted residue.
    fn entry_confirmed_absent(&self, dir: RawDirectory, name: &str) -> bool {
        // Drop the cache first so the lookup reads true on-disk state.
        self.drop_block_cache();
        matches!(
            self.mgr.find_directory_entry(dir, name),
            Err(embedded_sdmmc::Error::NotFound)
        )
    }

    /// Delete `name` in `dir`, reporting [`FsOpError::DeleteIncomplete`]
    /// when the delete still fails after `truncated` destruction has
    /// already persisted.
    ///
    /// The retries are cache-aware: a failed attempt may have left its own
    /// modified directory block in the manager's one-block cache, so a bare
    /// retry would read that echo back and see the entry as already deleted
    /// while the card never acknowledged the write. Dropping the cache
    /// (`device()` invalidates it) makes each retry re-read the true
    /// on-disk block -- after which a NotFound means the earlier attempt's
    /// write reached the card (a late-status error) and the delete won,
    /// once [`Self::entry_confirmed_absent`] rules out a laundered read
    /// error.
    fn delete_entry_checked(
        &self,
        dir: RawDirectory,
        name: &str,
        truncated: bool,
    ) -> Result<(), FsOpError<D::Error>> {
        // First attempt: a *confirmed* NotFound is the caller's genuine
        // "no such entry" answer -- unless the truncate just persisted, in
        // which case the entry provably existed moments ago and a confirmed
        // absence with no delete write issued is almost certainly a
        // double-laundered read; the honest report is then the emptied
        // ghost, never "file not found".
        let mut outcome = match self.mgr.delete_entry_in_dir(dir, name) {
            Err(error @ embedded_sdmmc::Error::NotFound)
                if self.entry_confirmed_absent(dir, name) =>
            {
                return Err(if truncated {
                    FsOpError::DeleteIncomplete(Box::new(error.into()))
                } else {
                    error.into()
                });
            }
            // Ok, a real error, or an unconfirmed NotFound (the entry is
            // still listed, or the confirming lookup itself failed).
            other => other,
        };
        for _ in 1..DELETE_RETRY_ATTEMPTS {
            if outcome.is_ok() {
                break;
            }
            self.drop_block_cache();
            outcome = match self.mgr.delete_entry_in_dir(dir, name) {
                Err(embedded_sdmmc::Error::NotFound) if self.entry_confirmed_absent(dir, name) => {
                    Ok(())
                }
                other => other,
            };
        }
        if outcome.is_err() {
            // The final failed attempt also left its echo in the cache;
            // drop it so later reads report the true on-disk state (the
            // entry still listed) instead of the phantom deletion.
            self.drop_block_cache();
        }
        outcome.map_err(|error| match error {
            // An unconfirmed NotFound: the delete claims the entry is
            // gone, but the lookup says otherwise (or could not tell).
            // Never report "file not found" for an entry that is still
            // listed.
            embedded_sdmmc::Error::NotFound if !truncated => FsOpError::VerifyFailed,
            error if truncated => FsOpError::DeleteIncomplete(Box::new(error.into())),
            error => FsOpError::from(error),
        })
    }

    pub fn read_directory_page(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<DirectoryItem>, usize), FsOpError<D::Error>> {
        self.list_directory_page(path, offset, limit, false, None)
    }

    pub fn read_directory_folders_page(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
        excluded_path: Option<&str>,
    ) -> Result<(Vec<DirectoryItem>, usize), FsOpError<D::Error>> {
        self.list_directory_page(path, offset, limit, true, excluded_path)
    }

    fn list_directory_page(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
        folders_only: bool,
        excluded_path: Option<&str>,
    ) -> Result<(Vec<DirectoryItem>, usize), FsOpError<D::Error>> {
        let mut page = Vec::with_capacity(limit);
        let mut total = 0;
        let mut lfn_storage = alloc::vec![0u8; LFN_BUFFER_BYTES];
        self.with_dir(path, |dir| {
            let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
            // Building items from the DirEntry alone matters: calling back
            // into the manager inside the iteration callback is a LockError.
            self.mgr
                .iterate_dir_lfn(dir, &mut lfn_buffer, |entry, lfn| {
                    if !is_listable(entry) || (folders_only && !entry.attributes.is_directory()) {
                        return ControlFlow::Continue(());
                    }
                    let entry_path = join_path(path, &entry.name.to_string());
                    if excluded_path
                        .is_some_and(|excluded| entry_path.eq_ignore_ascii_case(excluded))
                    {
                        return ControlFlow::Continue(());
                    }
                    if total >= offset && page.len() < limit {
                        page.push(DirectoryItem {
                            name: match lfn {
                                Some(long) if !long.is_empty() => long.into(),
                                _ => entry.name.to_string(),
                            },
                            path: entry_path,
                            is_dir: entry.attributes.is_directory(),
                            size: u64::from(entry.size),
                            hidden: entry.attributes.is_hidden(),
                            system: entry.attributes.is_system(),
                        });
                    }
                    total += 1;
                    ControlFlow::Continue(())
                })?;
            Ok(())
        })?;
        Ok((page, total))
    }

    /// Position of `target_path` in the listing order of `dir_path`, counting
    /// exactly like [`Self::read_directory_page`] so the explorer can page to
    /// a just-created entry.
    pub fn entry_index(
        &self,
        dir_path: &str,
        target_path: &str,
    ) -> Result<Option<usize>, FsOpError<D::Error>> {
        let mut index = None;
        let mut at = 0;
        self.with_dir(dir_path, |dir| {
            self.mgr.iterate_dir(dir, |entry| {
                if !is_listable(entry) {
                    return ControlFlow::Continue(());
                }
                if join_path(dir_path, &entry.name.to_string()).eq_ignore_ascii_case(target_path) {
                    index = Some(at);
                    return ControlFlow::Break(());
                }
                at += 1;
                ControlFlow::Continue(())
            })?;
            Ok(())
        })?;
        Ok(index)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsOpError<D::Error>> {
        let file = self.open_file_at(path, Mode::ReadOnly)?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 512];
        let result = loop {
            match self.mgr.read(file, &mut chunk) {
                Ok(0) => break Ok(()),
                Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                Err(error) => break Err(FsOpError::from(error)),
            }
        };
        let _ = self.mgr.close_file(file);
        result?;
        Ok(bytes)
    }

    /// Read an entire file into a buffer with the alignment required by
    /// rkyv archives. There is deliberately no document-size policy here;
    /// allocation and filesystem limits are the practical bounds.
    pub fn read_file_aligned(
        &self,
        path: &str,
    ) -> Result<rkyv::util::AlignedVec, FsOpError<D::Error>> {
        let entry = self.find_entry(path)?;
        if entry.attributes.is_directory() {
            return Err(FsOpError::IsADirectory);
        }
        let file = self.open_file_at(path, Mode::ReadOnly)?;
        let mut bytes = rkyv::util::AlignedVec::with_capacity(entry.size as usize);
        let mut chunk = [0u8; 512];
        let result = loop {
            match self.mgr.read(file, &mut chunk) {
                Ok(0) => break Ok(()),
                Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                Err(error) => break Err(FsOpError::from(error)),
            }
        };
        let _ = self.mgr.close_file(file);
        result?;
        Ok(bytes)
    }

    pub fn create_empty(&self, path: &str) -> Result<(), FsOpError<D::Error>> {
        self.mark_free_space_stale();
        self.with_dir(&parent_path(path), |dir| {
            let leaf = leaf_name(path);
            // Single read, unlike destructive callers of find_entry: a
            // laundered NotFound here is self-correcting, because the
            // creating open re-scans the directory and surfaces
            // AlreadyExists itself.
            match self.mgr.find_directory_entry(dir, leaf) {
                Ok(_) => return Err(FsOpError::AlreadyExists),
                Err(embedded_sdmmc::Error::NotFound) => {}
                Err(error) => return Err(error.into()),
            }
            // No readback verification: a follow-up read through the same
            // mounted filesystem can serve the intended state from its cache
            // even when that state never reached the card, masking the write
            // error.
            let file = self
                .mgr
                .open_file_in_dir(dir, leaf, Mode::ReadWriteCreate)?;
            self.mgr.close_file(file)?;
            Ok(())
        })
    }

    pub fn create_directory_verified(&self, path: &str) -> Result<(), FsOpError<D::Error>> {
        self.mark_free_space_stale();
        self.with_dir(&parent_path(path), |dir| {
            let leaf = leaf_name(path);
            // Single read: a laundered NotFound here is self-correcting,
            // because make_dir_in_dir re-scans the directory and surfaces
            // AlreadyExists itself.
            match self.mgr.find_directory_entry(dir, leaf) {
                Ok(_) => return Err(FsOpError::AlreadyExists),
                Err(embedded_sdmmc::Error::NotFound) => {}
                Err(error) => return Err(error.into()),
            }
            self.mgr.make_dir_in_dir(dir, leaf)?;
            // Confirmed: a single laundered read here would report the
            // just-created directory as "file not found".
            match self.confirmed_lookup(|| self.mgr.find_directory_entry(dir, leaf)) {
                Ok(entry) if entry.attributes.is_directory() => Ok(()),
                Ok(_) => Err(FsOpError::VerifyFailed),
                Err(error) => Err(error.into()),
            }
        })
    }

    /// Whether `path` names an existing entry. Short names have no case, so
    /// unlike the old case-exact check this is inherently case-insensitive.
    /// A `NotFound` is confirmed by a second, cache-dropped lookup before
    /// being reported as absence (see [`Self::find_entry`]): callers such as
    /// the save's staging-name scan act destructively on the answer.
    pub fn entry_exists(&self, path: &str) -> Result<bool, FsOpError<D::Error>> {
        match self.find_entry(path) {
            Ok(_) => Ok(true),
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Rename or move an entry. The stored filesystem has no rename
    /// operation, so this copies to the destination name and then deletes the
    /// source; a folder is copied recursively. The destination is never
    /// replaced.
    pub fn move_entry_verified(&self, from: &str, to: &str) -> Result<(), FsOpError<D::Error>> {
        self.mark_free_space_stale();
        let from_parent = parent_path(from);
        let to_parent = parent_path(to);
        let from_sfn = ShortFileName::create_from_str(leaf_name(from))
            .map_err(|error| FsOpError::Fs(embedded_sdmmc::Error::FilenameError(error)))?;
        let to_sfn = ShortFileName::create_from_str(leaf_name(to))
            .map_err(|error| FsOpError::Fs(embedded_sdmmc::Error::FilenameError(error)))?;
        if from_parent.eq_ignore_ascii_case(&to_parent) && from_sfn == to_sfn {
            // 8.3 names have no case, so renaming an entry to its own name
            // cannot change anything; succeed once the source is confirmed.
            self.find_entry(from)?;
            return Ok(());
        }
        if path_is_inside(from, to) {
            return Err(FsOpError::MoveIntoSelf);
        }
        let entry = self.find_entry(from)?;
        if entry.attributes.is_read_only() {
            // A move re-creates the entry writable and deletes the protected
            // original; honor the bit the same way save does.
            return Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly));
        }
        if entry_is_root_alias_debris(&entry) {
            // An interrupted make_dir's debris aliases the volume root:
            // copying it would clone the whole card into the destination
            // and the source delete would then walk the real root.
            return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster));
        }
        match self.find_entry(to) {
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(FsOpError::AlreadyExists),
        }
        if entry.attributes.is_directory() {
            // Refusing debris here matters: copy_dir_children hard-fails on
            // it, so waving it through would mutate the destination for an
            // operation that is guaranteed to abort midway.
            self.check_subtree_mutable(from, 0, true)?;
            // Root creation is hoisted out of the recursion: it is the one
            // step whose AlreadyExists proves this operation created
            // nothing, so it must be distinguishable from a child
            // collision met after the root landed. The pre-check above
            // proved `to` absent only as far as two laundered reads
            // allow, and make_dir's own scan is single-read
            // launder-prone, so this step's failure cleanup removes the
            // destination only when it is provably this operation's own
            // debris (see cleanup_failed_dir_creation).
            if let Err(error) = self.with_dir(&to_parent, |dir| {
                self.mgr
                    .make_dir_in_dir(dir, leaf_name(to))
                    .map_err(FsOpError::from)
            }) {
                return Err(self.cleanup_partial_destination(error, false, || {
                    self.cleanup_failed_dir_creation(to)
                }));
            }
            if let Err(error) = self.copy_dir_children(from, to, 0) {
                // Every failure here -- including AlreadyExists, which
                // assess_failure alone refuses to roll back -- strands
                // debris under a root this operation created, so it earns
                // the carve-out parameter. The guarded delete acts only on
                // a confirmed-unique destination name (a laundered
                // pre-check plus make_dir's laundered scan can have
                // appended a true duplicate 8.3 entry, and a by-name
                // delete may then resolve to the user's pre-existing
                // tree); a refused or failed cleanup reports the
                // retry-blocking debris as CleanupIncomplete. Destruction
                // progress is irrelevant on these paths, exactly as it is
                // for the source delete below.
                return Err(self.cleanup_partial_destination(error, true, || {
                    self.guarded_rollback_delete(to, true)
                }));
            }
            if let Err(error) = self.delete_recursive(from, 0, &mut false) {
                return Err(Self::wrap_source_delete_failure(error));
            }
        } else {
            self.copy_file_verified(from, to)?;
            if let Err(error) = self.delete_file_at(from) {
                return Err(Self::wrap_source_delete_failure(error));
            }
        }
        // Both re-checks are advisory: the destination provably holds a
        // byte-verified copy and the source delete already confirmed its
        // removal internally, so a verification read flake must never
        // convert a landed move into a reported failure (that invites the
        // user to "clean up" the only good copy).
        match self.find_entry(from) {
            // Still listed after its delete reported success: success with
            // a caveat, never "<verb> failed".
            Ok(_) => return Err(FsOpError::SourceRemains(Box::new(FsOpError::VerifyFailed))),
            // Confirmed absence, or the verification read itself failed.
            Err(_) => {}
        }
        match self.find_entry(to) {
            Ok(_) => Ok(()),
            // A *confirmed* absence of the destination that held a verified
            // copy moments ago: report verification failure, never "file
            // not found".
            Err(error) if error.is_not_found() => Err(FsOpError::VerifyFailed),
            // Verification read flake after a fully-landed move.
            Err(_) => Ok(()),
        }
    }

    /// Post-copy source-delete failures are success-with-caveat: the
    /// destination already holds the verified copy and must never be rolled
    /// back (after a truncate it may be the only good copy). Keep
    /// [`FsOpError::DeleteIncomplete`]'s precise "emptied ghost" report and
    /// wrap every other failure as [`FsOpError::SourceRemains`], so no
    /// caller reports "<verb> failed" while the move actually landed.
    fn wrap_source_delete_failure(error: FsOpError<D::Error>) -> FsOpError<D::Error> {
        match error {
            error @ FsOpError::DeleteIncomplete(_) => error,
            other => FsOpError::SourceRemains(Box::new(other)),
        }
    }

    /// Assess a failed copy (`assess_failure` refuses further mutations
    /// after proven or plausibly masked device faults, and normally
    /// refuses outright for `AlreadyExists`, which proves the copy
    /// created nothing worth removing), run the partial-destination
    /// `cleanup` when rollback is safe so a retry is not refused as
    /// AlreadyExists, and report a blocked cleanup as
    /// [`FsOpError::CleanupIncomplete`] so the retry-blocking ghost is
    /// surfaced instead of swallowed.
    ///
    /// `rollback_already_exists` is the one carve-out from
    /// `assess_failure`'s AlreadyExists rule: a child collision met
    /// *after* a move's destination root landed strands debris under a
    /// root this operation created, so its cleanup must run anyway. When
    /// that cleanup completes, nothing bearing the destination name
    /// remains and "already exists" would be false, so the collision is
    /// re-tagged [`FsOpError::CopyCollision`].
    fn cleanup_partial_destination(
        &self,
        error: FsOpError<D::Error>,
        rollback_already_exists: bool,
        cleanup: impl FnOnce() -> CleanupOutcome,
    ) -> FsOpError<D::Error> {
        let (error, rollback) = self.assess_failure(error);
        let collision = rollback_already_exists && matches!(error, FsOpError::AlreadyExists);
        if rollback || collision {
            if cleanup() == CleanupOutcome::Blocked {
                return FsOpError::CleanupIncomplete(Box::new(error));
            }
            if collision {
                return FsOpError::CopyCollision;
            }
        }
        error
    }

    /// Delete whatever uniquely bears `to`'s name, for rollback cleanups.
    /// Absence confirmed by both of the resolver's walks is clean: the
    /// failed operation never created its destination (the old "NotFound
    /// from cleanup is clean" answer, now confirmed instead of assumed).
    /// A unique match is deleted only when its kind matches what the
    /// failed operation created (`expect_dir`) -- a file rollback must
    /// never recursively delete a directory it cannot have made -- and
    /// interrupted-make_dir debris gets its surgical removal instead of
    /// a walk that would alias the volume root. Ambiguity, a kind
    /// mismatch, or a failed delete refuses: better a stranded ghost
    /// blocking retries than a by-name delete resolving to a
    /// pre-existing entry.
    fn guarded_rollback_delete(&self, to: &str, expect_dir: bool) -> CleanupOutcome {
        match self.resolve_destination_name_retried(to) {
            NameResolution::Absent => CleanupOutcome::Clean,
            NameResolution::Unsure => CleanupOutcome::Blocked,
            NameResolution::Unique(entry) => {
                // Checked before the kind: debris is always a directory
                // entry, whatever this operation was creating.
                if entry_is_root_alias_debris(&entry) {
                    return self.erase_root_alias_debris_entry(to, &entry).0;
                }
                if entry.attributes.is_directory() != expect_dir {
                    return CleanupOutcome::Blocked;
                }
                let deleted = if expect_dir {
                    self.delete_recursive(to, 0, &mut false)
                } else {
                    self.delete_file_at(to)
                };
                match deleted {
                    Ok(()) => CleanupOutcome::Clean,
                    Err(error) if error.is_not_found() => CleanupOutcome::Clean,
                    Err(_) => CleanupOutcome::Blocked,
                }
            }
        }
    }

    /// Cleanup for a move's failed destination-root creation. That step
    /// can have persisted at most a cluster-0 debris entry (`make_dir`
    /// writes the parent entry before allocating the directory's
    /// cluster), so that -- and only that -- is removed. Anything else
    /// uniquely bearing the name is refused: a real-cluster directory,
    /// even an empty one, is indistinguishable from a pre-existing user
    /// directory that the laundered pre-checks failed to see, and
    /// deleting it would destroy user data. The caller then reports
    /// [`FsOpError::CleanupIncomplete`].
    fn cleanup_failed_dir_creation(&self, to: &str) -> CleanupOutcome {
        match self.resolve_destination_name_retried(to) {
            NameResolution::Absent => CleanupOutcome::Clean,
            NameResolution::Unique(entry) if entry_is_root_alias_debris(&entry) => {
                self.erase_root_alias_debris_entry(to, &entry).0
            }
            _ => CleanupOutcome::Blocked,
        }
    }

    /// Remove a provably-ours interrupted-`make_dir` debris entry (see
    /// [`entry_is_root_alias_debris`]) by marking its 32-byte slot unused
    /// (`0xE5`). The manager cannot: opening the entry aliases the volume
    /// root, so its own delete walks the wrong directory to prove
    /// emptiness. The mark happens in a single device borrow -- which
    /// invalidates the manager's one-block cache on entry -- and the
    /// exact on-disk bytes are re-verified inside that borrow before the
    /// write, so a stale `entry` fails safe. Must not be called with any
    /// directory or file handle open (the borrow holds the manager's
    /// RefCell for the whole closure).
    ///
    /// The second return value is whether the erase write was acknowledged
    /// by the device: destruction has then (at least possibly) persisted
    /// even when the confirming lookup cannot prove the name gone -- a
    /// same-named twin entry, or a failed read -- so delete-path callers
    /// must report a Blocked-after-acknowledged outcome as "incomplete",
    /// never as a refusal implying nothing was touched. Rollback callers
    /// fold both into [`FsOpError::CleanupIncomplete`], which already
    /// carries that meaning.
    fn erase_root_alias_debris_entry(&self, to: &str, entry: &DirEntry) -> (CleanupOutcome, bool) {
        let offset = entry.entry_offset as usize;
        if offset % 32 != 0 || offset + 32 > 512 {
            return (CleanupOutcome::Blocked, false);
        }
        let lba = entry.entry_block.0;
        let erased = self.mgr.device(|device| {
            let Some(mut sector) = read_device_sector(device, lba) else {
                return false;
            };
            let on_disk = embedded_sdmmc::fat::OnDiskDirEntry::new(&sector[offset..offset + 32]);
            let is_debris = on_disk.is_valid()
                && on_disk.matches(&entry.name)
                && on_disk.raw_attr() & Attributes::LFN != Attributes::LFN
                && on_disk.raw_attr() & Attributes::DIRECTORY != 0
                && on_disk.first_cluster_hi() == 0
                && on_disk.first_cluster_lo() == 0;
            if !is_debris {
                return false;
            }
            // Foreign-created debris can carry LFN entries ahead of the
            // short-name slot (this firmware writes short names only);
            // erasing just the short name would orphan them onto whatever
            // entry next reuses the freed slot. Mark every immediately
            // preceding, checksum-matched LFN slot in this sector too --
            // same single-sector write, so atomicity is unchanged. A
            // chain crossing into an earlier sector is rare and left as
            // a harmless orphan for a PC disk check.
            let checksum = sector[offset..offset + 11].iter().fold(0u8, |sum, &byte| {
                ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte)
            });
            let mut slot = offset;
            while slot >= 32 {
                let lfn = slot - 32;
                if sector[lfn] == 0x00
                    || sector[lfn] == 0xE5
                    || sector[lfn + 11] != Attributes::LFN
                    || sector[lfn + 13] != checksum
                {
                    break;
                }
                sector[lfn] = 0xE5;
                slot = lfn;
            }
            sector[offset] = 0xE5;
            write_device_sector(device, lba, &sector)
        });
        if !erased {
            return (CleanupOutcome::Blocked, false);
        }
        // The write bypassed the manager; the confirming lookup (and every
        // later read) must fetch true on-disk state, not the pre-surgery
        // block.
        self.drop_block_cache();
        match self.find_entry(to) {
            Err(error) if error.is_not_found() => (CleanupOutcome::Clean, true),
            // Still sighted, or the confirming read failed: the *name's*
            // fate is unproven, but the acknowledged erase stands.
            _ => (CleanupOutcome::Blocked, true),
        }
    }

    /// Resolve what uniquely bears `to`'s name in its parent, for rollback
    /// cleanups that must delete by name. The confirmed-absence pre-check
    /// plus a creating open's own (single-read, launder-prone) scan can
    /// both be blinded by a FAT-region outage, after which a copy or
    /// make_dir appends a true duplicate 8.3 entry and every by-name
    /// resolution reaches whichever entry lists first -- so a by-name
    /// delete may only act on a *unique* match. The walk itself launders
    /// (chain-step read errors truncate the listing), so the count is
    /// asked twice with the block cache dropped between, matching the
    /// two-agreeing-reads standard of [`Self::confirmed_lookup`]: a wrong
    /// verdict needs two laundered walks in a row, the accepted residue.
    /// Unique matches must also agree on the on-disk slot, so two
    /// different same-named entries seen once each never pass as one.
    /// [`Self::resolve_destination_name`] with flake tolerance: an Unsure
    /// verdict is re-asked -- cache dropped between attempts, up to
    /// [`RESOLVE_RETRY_ATTEMPTS`] total -- before a caller refuses on it.
    /// A transient flake makes the resolver's two walks disagree once and
    /// heals on the re-ask; a genuine duplicate name or an unreadable
    /// parent answers Unsure every time and still ends refused, so
    /// retrying never weakens the guard.
    fn resolve_destination_name_retried(&self, to: &str) -> NameResolution {
        let mut resolution = self.resolve_destination_name(to);
        for _ in 1..RESOLVE_RETRY_ATTEMPTS {
            if !matches!(resolution, NameResolution::Unsure) {
                break;
            }
            self.drop_block_cache();
            resolution = self.resolve_destination_name(to);
        }
        resolution
    }

    fn resolve_destination_name(&self, to: &str) -> NameResolution {
        let Ok(sfn) = ShortFileName::create_from_str(leaf_name(to)) else {
            return NameResolution::Unsure;
        };
        let walks = self.with_dir(&parent_path(to), |dir| {
            let first = self.count_name_matches(dir, &sfn);
            self.drop_block_cache();
            let second = self.count_name_matches(dir, &sfn);
            Ok((first, second))
        });
        match walks {
            Ok((Ok((0, _)), Ok((0, _)))) => NameResolution::Absent,
            Ok((Ok((1, Some(first))), Ok((1, Some(second)))))
                if first.entry_block == second.entry_block
                    && first.entry_offset == second.entry_offset =>
            {
                NameResolution::Unique(second)
            }
            // The parent itself is confirmedly absent (with_dir's
            // component lookups are confirmed_lookup double-checked),
            // which proves the name absent too: the failed operation
            // created nothing, so its cleanup is clean -- the same
            // answer the file arm's created=false path gives.
            Err(error) if error.is_not_found() => NameResolution::Absent,
            _ => NameResolution::Unsure,
        }
    }

    /// One allocation-free pass over an open directory: how many entries
    /// bear `sfn` (stopping at two -- callers never distinguish further),
    /// plus the listable match itself. Every same-named entry counts,
    /// whatever its attributes: the by-name delete a Unique verdict
    /// authorizes matches raw 11-byte names with no attribute filter, so
    /// an uncounted match -- a volume label sharing the name of a
    /// root-level file -- would be the entry the delete actually erases.
    /// A non-listable match therefore counts toward ambiguity but is
    /// never returned as the match. A pure counter, so the manager is
    /// never re-entered from its own callback.
    fn count_name_matches(
        &self,
        dir: RawDirectory,
        sfn: &ShortFileName,
    ) -> Result<(u8, Option<DirEntry>), embedded_sdmmc::Error<D::Error>> {
        let mut count = 0u8;
        let mut found = None;
        self.mgr.iterate_dir(dir, |entry| {
            if entry.name == *sfn {
                count += 1;
                if is_listable(entry) {
                    found = Some(entry.clone());
                }
                if count >= 2 {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        })?;
        Ok((count, found))
    }

    /// Copy a file and prove the destination matches the source, removing a
    /// partial destination on failure (unless the device itself failed, after
    /// which no further mutations are trustworthy). The removal is guarded:
    /// it acts only on a confirmed-unique destination name, because the
    /// creating open's launder-prone scan can have appended a duplicate 8.3
    /// entry whose by-name delete would resolve to the user's pre-existing
    /// file. A refused or failed removal leaves a ghost that will block
    /// retries as AlreadyExists, so it surfaces as
    /// [`FsOpError::CleanupIncomplete`] instead of being swallowed.
    fn copy_file_verified(&self, from: &str, to: &str) -> Result<(), FsOpError<D::Error>> {
        let mut created = false;
        self.copy_file_contents(from, to, Mode::ReadWriteCreate, &mut created)
            .map_err(|error| {
                self.cleanup_partial_destination(error, false, || {
                    if created {
                        self.guarded_rollback_delete(to, false)
                    } else {
                        // The creating open itself failed: nothing was
                        // created, and whatever uniquely bears the name --
                        // if anything -- is a pre-existing file the
                        // laundered pre-checks failed to see. There is
                        // nothing to clean, and a by-name delete would
                        // destroy user data.
                        CleanupOutcome::Clean
                    }
                })
            })
    }

    /// Streams `from` into `to` through a fixed window and then proves the
    /// copy with a byte compare. `dest_mode` decides whether an existing
    /// destination is an error (`ReadWriteCreate`) or gets overwritten
    /// (`ReadWriteCreateOrTruncate`, used when restoring from a backup).
    /// `destination_created` reports whether the destination open
    /// succeeded, so a failure's cleanup can tell "our partial file
    /// exists" from "nothing was ever created" -- the rollback-delete
    /// decision turns on it.
    fn copy_file_contents(
        &self,
        from: &str,
        to: &str,
        dest_mode: Mode,
        destination_created: &mut bool,
    ) -> Result<(), FsOpError<D::Error>> {
        let source = self.open_file_at(from, Mode::ReadOnly)?;
        let copied: Result<(), FsOpError<D::Error>> = (|| {
            let destination = self.open_file_at(to, dest_mode)?;
            *destination_created = true;
            let mut buffer = alloc::vec![0u8; COPY_CHUNK_BYTES];
            let streamed = loop {
                match self.mgr.read(source, &mut buffer) {
                    Ok(0) => break Ok(()),
                    Ok(count) => {
                        if let Err(error) = self.mgr.write(destination, &buffer[..count]) {
                            break Err(FsOpError::from(error));
                        }
                    }
                    Err(error) => break Err(FsOpError::from(error)),
                }
            };
            let close_result = self.mgr.close_file(destination);
            streamed?;
            close_result?;
            Ok(())
        })();
        let _ = self.mgr.close_file(source);
        copied?;
        self.compare_files(from, to)
    }

    /// Chunked byte comparison of two closed files.
    fn compare_files(&self, left: &str, right: &str) -> Result<(), FsOpError<D::Error>> {
        let left_file = self.open_file_at(left, Mode::ReadOnly)?;
        let result: Result<(), FsOpError<D::Error>> = (|| {
            let right_file = self.open_file_at(right, Mode::ReadOnly)?;
            let mut left_buffer = alloc::vec![0u8; COPY_CHUNK_BYTES];
            let mut right_buffer = alloc::vec![0u8; COPY_CHUNK_BYTES];
            let compared = loop {
                let left_count = match self.read_full(left_file, &mut left_buffer) {
                    Ok(count) => count,
                    Err(error) => break Err(error),
                };
                let right_count = match self.read_full(right_file, &mut right_buffer) {
                    Ok(count) => count,
                    Err(error) => break Err(error),
                };
                if left_count != right_count
                    || left_buffer[..left_count] != right_buffer[..right_count]
                {
                    break Err(FsOpError::VerifyFailed);
                }
                if left_count == 0 {
                    break Ok(());
                }
            };
            let _ = self.mgr.close_file(right_file);
            compared
        })();
        let _ = self.mgr.close_file(left_file);
        result
    }

    fn read_full(&self, file: RawFile, buffer: &mut [u8]) -> Result<usize, FsOpError<D::Error>> {
        let mut done = 0;
        while done < buffer.len() {
            let count = self.mgr.read(file, &mut buffer[done..])?;
            if count == 0 {
                break;
            }
            done += count;
        }
        Ok(done)
    }

    fn copy_dir_recursive(
        &self,
        from: &str,
        to: &str,
        depth: usize,
    ) -> Result<(), FsOpError<D::Error>> {
        if depth >= MAX_TREE_DEPTH {
            return Err(FsOpError::TooDeep);
        }
        self.with_dir(&parent_path(to), |dir| {
            self.mgr
                .make_dir_in_dir(dir, leaf_name(to))
                .map_err(FsOpError::from)
        })?;
        self.copy_dir_children(from, to, depth)
    }

    /// The children half of [`Self::copy_dir_recursive`]: the caller has
    /// already created (and thereby owns) the destination root, so every
    /// failure escaping here implies destination-side debris exists.
    /// [`Self::move_entry_verified`] relies on that split to tell a root
    /// creation refused as AlreadyExists (nothing created) from a child
    /// collision (partial tree stranded).
    fn copy_dir_children(
        &self,
        from: &str,
        to: &str,
        depth: usize,
    ) -> Result<(), FsOpError<D::Error>> {
        // The source is never mutated, so plain offset pagination visits
        // every child exactly once with one batch resident at a time.
        let mut skip = 0;
        loop {
            let (batch, more) = self.list_children_bounded(from, skip, CHILD_BATCH)?;
            skip += batch.len();
            for child in &batch {
                if child.aliases_root {
                    // Interrupted-make_dir debris in the source: copying
                    // "its" children would clone the real volume root.
                    // Refuse the whole operation; the source needs repair
                    // (deleting the debris) first.
                    return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster));
                }
                let child_from = join_path(from, &child.name);
                let child_to = join_path(to, &child.name);
                if child.is_dir {
                    self.copy_dir_recursive(&child_from, &child_to, depth + 1)?;
                } else {
                    self.copy_file_verified(&child_from, &child_to)?;
                }
            }
            if !more {
                break;
            }
        }
        Ok(())
    }

    fn delete_recursive(
        &self,
        path: &str,
        depth: usize,
        destroyed: &mut bool,
    ) -> Result<(), FsOpError<D::Error>> {
        if depth >= MAX_TREE_DEPTH {
            return Err(FsOpError::TooDeep);
        }
        loop {
            // Always re-list from the front: deleting the batch is what
            // advances the walk, and every non-empty batch shrinks the
            // directory, so the loop terminates.
            let (batch, _more) = self.list_children_bounded(path, 0, CHILD_BATCH)?;
            if batch.is_empty() {
                break;
            }
            // All file children of a batch go through one held parent handle
            // instead of re-walking the path from the root per file.
            self.with_dir(path, |dir| {
                for child in batch.iter().filter(|child| !child.is_dir) {
                    if child.read_only {
                        // Defense in depth: callers pre-scan and refuse
                        // read-only subtrees before mutating anything.
                        return Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly));
                    }
                    let result = self.delete_file_in_dir(dir, child.name.as_str());
                    // Ok means the entry is gone; DeleteIncomplete means the
                    // truncate persisted. Either way destruction has begun,
                    // and any later failure must not claim nothing was
                    // deleted.
                    if matches!(result, Ok(()) | Err(FsOpError::DeleteIncomplete(_))) {
                        *destroyed = true;
                    }
                    result?;
                }
                Ok(())
            })?;
            // The handle above is closed before recursing, so nesting never
            // stacks directory handles against the manager's limits.
            for child in batch.iter().filter(|child| child.is_dir) {
                let child_path = join_path(path, &child.name);
                if child.aliases_root {
                    // Interrupted-make_dir debris: recursing would walk --
                    // and delete children of -- the real volume root. Remove
                    // the bare entry surgically instead.
                    match self.find_entry(&child_path) {
                        Ok(entry) if entry_is_root_alias_debris(&entry) => {
                            let (outcome, erase_acknowledged) =
                                self.erase_root_alias_debris_entry(&child_path, &entry);
                            if erase_acknowledged {
                                // The erase write reached the card even if
                                // the confirming lookup could not prove the
                                // name gone; a failure below must surface
                                // as "incomplete", never as a refusal.
                                *destroyed = true;
                            }
                            if outcome == CleanupOutcome::Blocked {
                                return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster));
                            }
                            continue;
                        }
                        // The listing said debris but the confirmed lookup
                        // disagrees (or failed): refuse rather than recurse
                        // into what may alias the root.
                        _ => return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster)),
                    }
                }
                self.delete_recursive(&child_path, depth + 1, destroyed)?;
            }
        }
        self.delete_file_at(path)?;
        // The entry's own removal is destruction too: when this frame is a
        // child of a larger walk, a later sibling's failure must surface as
        // "incomplete", never as a refusal implying the subtree is intact.
        *destroyed = true;
        Ok(())
    }

    /// Refuse subtree mutations that would otherwise fail midway: any
    /// read-only entry (this filesystem has no way to clear the bit) or
    /// nesting past [`MAX_TREE_DEPTH`] anywhere below `path` is reported
    /// before a single entry is copied or deleted. Read-only walk.
    ///
    /// Interrupted-make_dir debris (which aliases the volume root and has
    /// nothing of its own below it) is never descended into; whether it is
    /// tolerated depends on the operation. Deletes skip it --
    /// delete_recursive removes the bare entry surgically -- while
    /// copy-based operations must refuse it here, with the same BadCluster
    /// their copy phase (copy_dir_children) would otherwise fail with only
    /// after the destination root and earlier children already landed.
    fn check_subtree_mutable(
        &self,
        path: &str,
        depth: usize,
        refuse_root_alias_debris: bool,
    ) -> Result<(), FsOpError<D::Error>> {
        if depth >= MAX_TREE_DEPTH {
            return Err(FsOpError::TooDeep);
        }
        let mut skip = 0;
        loop {
            let (batch, more) = self.list_children_bounded(path, skip, CHILD_BATCH)?;
            skip += batch.len();
            for child in &batch {
                if child.read_only {
                    return Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly));
                }
                if child.aliases_root {
                    if refuse_root_alias_debris {
                        return Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster));
                    }
                    continue;
                }
                if child.is_dir {
                    self.check_subtree_mutable(
                        &join_path(path, &child.name),
                        depth + 1,
                        refuse_root_alias_debris,
                    )?;
                }
            }
            if !more {
                break;
            }
        }
        Ok(())
    }

    /// Up to `limit` listable children of `path` after skipping `skip`, plus
    /// whether more remain. Entries are copied out inside the iteration (the
    /// manager cannot be re-entered from its own callback), and every
    /// consumer acts on the batch only after all handles are closed, so
    /// subtree recursion never stacks directory handles against the
    /// manager's limits and never holds more than one batch per level in
    /// memory. Skipping the `.` and `..` dot entries here is what stops
    /// recursions looping forever.
    fn list_children_bounded(
        &self,
        path: &str,
        skip: usize,
        limit: usize,
    ) -> Result<(Vec<ChildEntry>, bool), FsOpError<D::Error>> {
        let mut children = Vec::with_capacity(limit);
        let mut seen = 0usize;
        let mut more = false;
        self.with_dir(path, |dir| {
            self.mgr.iterate_dir(dir, |entry| {
                if !is_listable(entry) {
                    return ControlFlow::Continue(());
                }
                if seen >= skip {
                    if children.len() < limit {
                        children.push(ChildEntry {
                            name: entry.name.to_string(),
                            is_dir: entry.attributes.is_directory(),
                            read_only: entry.attributes.is_read_only(),
                            aliases_root: entry_is_root_alias_debris(entry),
                        });
                    } else {
                        more = true;
                        return ControlFlow::Break(());
                    }
                }
                seen += 1;
                ControlFlow::Continue(())
            })?;
            Ok(())
        })?;
        Ok((children, more))
    }

    pub fn delete_verified(&self, path: &str, is_dir: bool) -> Result<(), FsOpError<D::Error>> {
        self.mark_free_space_stale();
        let entry = self.find_entry(path)?;
        if entry.attributes.is_directory() != is_dir {
            return Err(if is_dir {
                FsOpError::NotADirectory
            } else {
                FsOpError::IsADirectory
            });
        }
        if entry.attributes.is_read_only() {
            // The stored filesystem has no way to clear the bit on-device,
            // so honor it: silently deleting a protected entry would defeat
            // its purpose.
            return Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly));
        }
        if entry_is_root_alias_debris(&entry) {
            // Interrupted-make_dir debris: both the read-only pre-scan and
            // the recursive delete would walk the real volume root (and the
            // latter would destroy its children). Remove the bare entry
            // surgically; the helper confirms the removal itself, so the
            // advisory re-check below is not needed.
            return match self.erase_root_alias_debris_entry(path, &entry) {
                (CleanupOutcome::Clean, _) => Ok(()),
                // The erase write was acknowledged but the confirming
                // lookup still sights the name (a same-named twin entry, or
                // a failed read): destruction persisted, so the honest
                // report is "incomplete", never a refusal implying the
                // entry is intact.
                (CleanupOutcome::Blocked, true) => Err(FsOpError::DeleteIncomplete(Box::new(
                    FsOpError::Fs(embedded_sdmmc::Error::BadCluster),
                ))),
                (CleanupOutcome::Blocked, false) => {
                    Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster))
                }
            };
        }
        if is_dir {
            // The read-only pre-scan destroys nothing; once the recursive
            // delete has removed any child -- file or subfolder entry -- a
            // later failure must surface as "incomplete", never as a
            // refusal implying the folder is intact (FAT16 has no truncate,
            // so per-file tracking alone would miss every removed child
            // there).
            self.check_subtree_mutable(path, 0, false)?;
            let mut destroyed = false;
            self.delete_recursive(path, 0, &mut destroyed)
                .map_err(|error| match error {
                    error @ FsOpError::DeleteIncomplete(_) => error,
                    other if destroyed => FsOpError::DeleteIncomplete(Box::new(other)),
                    other => other,
                })?;
        } else {
            self.delete_file_at(path)?;
        }
        // Advisory re-check: delete_entry_checked already double-confirmed
        // the removal (or its write was acknowledged), so a verification
        // read flake must never convert the completed delete into a
        // "Not deleted" report.
        match self.find_entry(path) {
            Err(error) if error.is_not_found() => Ok(()),
            // A positive sighting after the delete reported success: at
            // minimum the truncate or child clearing persisted, so the
            // honest report is "incomplete", never a refusal.
            Ok(_) => Err(FsOpError::DeleteIncomplete(Box::new(
                FsOpError::VerifyFailed,
            ))),
            // The verification read itself failed; the delete stands.
            Err(_) => Ok(()),
        }
    }

    /// Replace a file's contents and verify every byte by reading it back.
    /// The close before verification matters: writes are durable only once
    /// the file is closed.
    pub fn save_verified(&self, path: &str, contents: &[u8]) -> Result<(), FsOpError<D::Error>> {
        self.save_verified_tracking(path, contents, &mut false)
    }

    /// [`Self::save_verified`] plus an `opened` report: whether the target
    /// open succeeded, so a transactional caller's rollback can tell "the
    /// target was touched" from "nothing happened" (the rollback-delete
    /// decision turns on it).
    fn save_verified_tracking(
        &self,
        path: &str,
        contents: &[u8],
        opened: &mut bool,
    ) -> Result<(), FsOpError<D::Error>> {
        self.mark_free_space_stale();
        self.write_file(path, contents, opened)?;
        self.verify_file(path, contents)
    }

    fn write_file(
        &self,
        path: &str,
        contents: &[u8],
        opened: &mut bool,
    ) -> Result<(), FsOpError<D::Error>> {
        let file = self.open_file_at(path, Mode::ReadWriteCreateOrTruncate)?;
        *opened = true;
        let write_result = self.mgr.write(file, contents).map_err(FsOpError::from);
        let close_result = self.mgr.close_file(file).map_err(FsOpError::from);
        write_result?;
        close_result
    }

    /// Chunked comparison of a closed file against an in-RAM image: a
    /// 512-byte stack window walks the file so verification never buffers a
    /// whole document on the heap.
    fn verify_file(&self, path: &str, expected: &[u8]) -> Result<(), FsOpError<D::Error>> {
        let file = self.open_file_at(path, Mode::ReadOnly)?;
        let result = (|| {
            let mut chunk = [0u8; 512];
            let mut done = 0usize;
            loop {
                let count = self.read_full(file, &mut chunk)?;
                if count == 0 {
                    break;
                }
                if expected.len() < done + count || chunk[..count] != expected[done..done + count] {
                    return Err(FsOpError::VerifyFailed);
                }
                done += count;
            }
            if done == expected.len() {
                Ok(())
            } else {
                Err(FsOpError::VerifyFailed)
            }
        })();
        let _ = self.mgr.close_file(file);
        result
    }

    /// Replace a file while keeping a proven copy of both the old and the new
    /// contents on the card at every step.
    ///
    /// The stored filesystem has no rename, so unlike the classic temp-file
    /// dance the target is rewritten in place (which also preserves an
    /// existing long filename): first the new contents are written and
    /// verified as `~WIO*.TMP`, then the current contents are copied and
    /// verified as `~WIO*.BAK`, and only then is the target touched. A power
    /// loss during the in-place rewrite leaves the target partial, but the
    /// old data survives in the backup and the new data in the temporary --
    /// both recoverable on a PC. That window, and needing free space for
    /// both staging copies (roughly twice the document size), are the cost
    /// of a rename-less filesystem.
    ///
    /// A failed save reports any surviving backup of the old contents via
    /// [`SaveError::backup_kept`], so the caller can point the user at it.
    pub fn save_transactional(
        &self,
        path: &str,
        contents: &[u8],
    ) -> Result<(), SaveError<D::Error>> {
        self.mark_free_space_stale();
        let directory = parent_path(path);
        // Confirmed lookup: a single laundered NotFound here would skip the
        // pre-checks below AND the backup staging, after which the in-place
        // rewrite destroys the only copy of an existing document.
        let existed = match self.find_entry(path) {
            Ok(entry) if entry.attributes.is_directory() => {
                return Err(FsOpError::IsADirectory.into());
            }
            Ok(entry) if entry.attributes.is_read_only() => {
                return Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly).into());
            }
            Ok(_) => true,
            Err(error) if error.is_not_found() => false,
            Err(error) => return Err(error.into()),
        };

        // Each probe's absence answer is confirmed by find_entry's in-dir
        // re-ask, so the scan's cost is bounded by two directory walks plus
        // two cache-dropped re-reads of the already-open parent.
        let mut suffix = 0u16;
        let (temporary, backup) = loop {
            let temporary = join_path(&directory, &alloc::format!("~WIO{suffix:04X}.TMP"));
            let backup = join_path(&directory, &alloc::format!("~WIO{suffix:04X}.BAK"));
            if !self.entry_exists(&temporary)? && !self.entry_exists(&backup)? {
                break (temporary, backup);
            }
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| SaveError::from(FsOpError::Fs(embedded_sdmmc::Error::DiskFull)))?;
        };

        // Prove the card can store the new contents before the original is
        // touched.
        if let Err(error) = self.save_verified(&temporary, contents) {
            // Do not perform more mutations after a device I/O error (proven,
            // or plausibly hiding behind the manager's DiskFull masking): the
            // mounted metadata cache no longer proves what reached the card.
            // Rollback deletes of *staging* names here and below are
            // deliberately best-effort (unlike copy/move's
            // CleanupIncomplete reporting): staging leftovers are tolerated
            // by design and surfaced via SaveError::backup_kept where they
            // matter, so a cleanup flake never converts a clean failure
            // report into a worse one. They still go through the guard,
            // because the temporary shares the backup's duplicate-name
            // hazard: a laundered scan inside write_file's creating open
            // can append a second 8.3 entry, after which a bare by-name
            // delete could truncate a previous failed save's preserved
            // temporary -- the only durable copy of that save's contents.
            // The one exception is debris at the target path itself, which
            // the new-file rollback below does report as CleanupIncomplete.
            let (error, rollback) = self.assess_failure(error);
            if rollback {
                let _ = self.guarded_rollback_delete(&temporary, false);
            }
            return Err(error.into());
        }

        // Keep the original contents reachable under the backup name for as
        // long as the target is at risk. The copy streams card-to-card and
        // verifies itself, so the old document never transits RAM.
        if existed {
            let mut backup_created = false;
            if let Err(error) =
                self.copy_file_contents(path, &backup, Mode::ReadWriteCreate, &mut backup_created)
            {
                let (error, rollback) = self.assess_failure(error);
                if rollback {
                    // Delete the backup name only when this save actually
                    // created it, and then only via the guard: the staging
                    // scan's confirmed absence can be double-laundered,
                    // after which this save's creating open appends a
                    // duplicate of a *pre-existing* backup (possibly one an
                    // earlier failure advertised via backup_kept), and a
                    // bare by-name delete could destroy it. A refused
                    // cleanup merely strands staging debris, which is
                    // tolerated by design.
                    if backup_created {
                        let _ = self.guarded_rollback_delete(&backup, false);
                    }
                    let _ = self.guarded_rollback_delete(&temporary, false);
                } else if matches!(error, FsOpError::AlreadyExists) {
                    // AlreadyExists proves the *backup* step created
                    // nothing, but the temporary is this operation's own
                    // verified creation and the device is healthy: remove
                    // it rather than stranding it.
                    let _ = self.guarded_rollback_delete(&temporary, false);
                }
                return Err(error.into());
            }
        }

        let mut target_opened = false;
        if let Err(error) = self.save_verified_tracking(path, contents, &mut target_opened) {
            let (mut error, rollback) = self.assess_failure(error);
            let mut backup_kept = None;
            if rollback {
                if existed {
                    // Restore the target from the on-card backup; the copy
                    // verifies itself, so success proves the original is back
                    // in place and the staging copies have served their
                    // purpose. Otherwise leave ~WIO*.TMP and ~WIO*.BAK
                    // behind: they are the only proven copies of the new and
                    // old contents.
                    if self
                        .copy_file_contents(
                            &backup,
                            path,
                            Mode::ReadWriteCreateOrTruncate,
                            &mut false,
                        )
                        .is_ok()
                    {
                        let _ = self.guarded_rollback_delete(&temporary, false);
                        // The verified restore proves the target holds the
                        // original, so the backup has served its purpose.
                        // The delete's result encodes destruction (see
                        // delete_file_in_dir), but DeleteIncomplete only
                        // proves destruction was POSSIBLE: the truncate's
                        // opening FAT read can fail before any FAT write,
                        // leaving the backup's full contents intact (see
                        // reclaim_file_clusters_in_dir). The restore just
                        // byte-verified the target against the backup, so
                        // reading the backup back settles it: equal means
                        // the truncate never landed and the full-content
                        // backup must be named; unequal or unreadable means
                        // a ghost or partial not worth burying the real
                        // error behind (naming requires proof of content --
                        // a compare flake on this already-faulty card
                        // suppresses, the same residue doctrine as every
                        // other correlated double fault). Any other failure
                        // refused before destroying anything -- on FAT16
                        // the truncate never runs at all -- so a
                        // full-content backup survives and must be named.
                        // The delete acts by name, so it may only run when
                        // the name confirmedly resolves to one file: a
                        // double-laundered staging scan can have appended
                        // this backup as a duplicate of a pre-existing
                        // (possibly advertised) one, and a bare delete
                        // would truncate whichever lists first. Ambiguity
                        // -- or an absence verdict contradicting the
                        // restore that just read the backup -- refuses and
                        // names the backup instead.
                        match self.resolve_destination_name_retried(&backup) {
                            NameResolution::Unique(entry) if !entry.attributes.is_directory() => {
                                match self.delete_file_at(&backup) {
                                    Ok(()) => {}
                                    Err(FsOpError::DeleteIncomplete(_)) => {
                                        if self.compare_files(&backup, path).is_ok() {
                                            backup_kept = Some(backup.clone());
                                        }
                                    }
                                    Err(_) => backup_kept = Some(backup.clone()),
                                }
                            }
                            _ => backup_kept = Some(backup.clone()),
                        }
                    } else {
                        backup_kept = Some(backup.clone());
                    }
                } else {
                    // Delete the target name only when the commit actually
                    // opened it, and then only via the guard: the confirmed
                    // "didn't exist" verdict can itself be double-laundered,
                    // with the commit's creating open having appended a
                    // duplicate of the real target -- a bare by-name delete
                    // would then truncate the pre-existing file. A refused
                    // or failed cleanup here is not a tolerated staging
                    // leftover: the debris is a partial file listed at the
                    // user's own path (which the next save would even stage
                    // as the "original"), so it is reported as
                    // CleanupIncomplete exactly as copy/move report theirs.
                    if target_opened
                        && self.guarded_rollback_delete(path, false) == CleanupOutcome::Blocked
                    {
                        error = FsOpError::CleanupIncomplete(Box::new(error));
                    }
                    let _ = self.guarded_rollback_delete(&temporary, false);
                }
            } else if existed {
                // No rollback: the target may be partial and both staging
                // files were deliberately left in place.
                backup_kept = Some(backup.clone());
            }
            return Err(SaveError { error, backup_kept });
        }

        // The target now contains verified data. A stale temporary or backup
        // is much less harmful than telling the user the save failed (or
        // rolling back good data) solely because cleanup had a transient card
        // error. The backup's delete is guarded like the rollback ones: a
        // duplicate of a pre-existing backup must strand, never resolve a
        // by-name delete onto the older file.
        let _ = self.guarded_rollback_delete(&temporary, false);
        if existed {
            let _ = self.guarded_rollback_delete(&backup, false);
        }
        Ok(())
    }

    /// Transactionally create a file that must still be absent when the
    /// first save commits. This is used after the new-document kind chooser:
    /// naming reserves nothing, so a file created in the meantime is never
    /// truncated or overwritten.
    pub fn save_transactional_create_new(
        &self,
        path: &str,
        contents: &[u8],
    ) -> Result<(), SaveError<D::Error>> {
        self.mark_free_space_stale();
        match self.find_entry(path) {
            Ok(_) => return Err(FsOpError::AlreadyExists.into()),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }

        let directory = parent_path(path);
        let mut suffix = 0u16;
        let temporary = loop {
            let temporary = join_path(&directory, &alloc::format!("~WIO{suffix:04X}.TMP"));
            let backup = join_path(&directory, &alloc::format!("~WIO{suffix:04X}.BAK"));
            if !self.entry_exists(&temporary)? && !self.entry_exists(&backup)? {
                break temporary;
            }
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| SaveError::from(FsOpError::Fs(embedded_sdmmc::Error::DiskFull)))?;
        };

        if let Err(error) = self.save_verified(&temporary, contents) {
            let (error, rollback) = self.assess_failure(error);
            if rollback {
                let _ = self.guarded_rollback_delete(&temporary, false);
            }
            return Err(error.into());
        }

        // Re-check immediately before a create-only open. Even if the check
        // is stale, ReadWriteCreate itself refuses an existing entry.
        match self.find_entry(path) {
            Ok(_) => {
                let _ = self.guarded_rollback_delete(&temporary, false);
                return Err(FsOpError::AlreadyExists.into());
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => {
                let _ = self.guarded_rollback_delete(&temporary, false);
                return Err(error.into());
            }
        }
        let file = match self.open_file_at(path, Mode::ReadWriteCreate) {
            Ok(file) => file,
            Err(error) => {
                let _ = self.guarded_rollback_delete(&temporary, false);
                return Err(error.into());
            }
        };
        let write_result = self.mgr.write(file, contents).map_err(FsOpError::from);
        let close_result = self.mgr.close_file(file).map_err(FsOpError::from);
        let commit = write_result
            .and(close_result)
            .and_then(|()| self.verify_file(path, contents));
        if let Err(error) = commit {
            let (mut error, rollback) = self.assess_failure(error);
            if rollback && self.guarded_rollback_delete(path, false) == CleanupOutcome::Blocked {
                error = FsOpError::CleanupIncomplete(Box::new(error));
            }
            let _ = self.guarded_rollback_delete(&temporary, false);
            return Err(error.into());
        }
        let _ = self.guarded_rollback_delete(&temporary, false);
        Ok(())
    }
}

/// Read one sector through a mounted volume's block device. Transient
/// per-command flakes are the device layer's problem (see
/// [`SD_RETRY_ATTEMPTS`]); a miss surfacing here blanks the figure.
fn read_device_sector<D: BlockDevice>(device: &D, lba: u32) -> Option<[u8; 512]> {
    let mut block = [Block::new()];
    if device.read(&mut block, BlockIdx(lba)).is_err() {
        return None;
    }
    let [block] = block;
    Some(block.contents)
}

/// Write one sector through a mounted volume's block device; same
/// transient-flake contract as [`read_device_sector`].
fn write_device_sector<D: BlockDevice>(device: &D, lba: u32, contents: &[u8; 512]) -> bool {
    let mut block = Block::new();
    block.contents = *contents;
    device.write(&[block], BlockIdx(lba)).is_ok()
}

/// Mark every exactly-zero (free) FAT entry in `tail` with `marker` (the
/// little-endian bad-cluster mark for the FAT width), leaving pre-existing
/// bad marks or foreign garbage untouched; returns whether anything
/// changed. Shared by the formatter and the mount-time repair so the two
/// can never diverge on what a marked tail looks like.
fn mark_phantom_tail<const N: usize>(tail: &mut [u8], marker: [u8; N]) -> bool {
    let mut changed = false;
    for entry in tail.as_chunks_mut::<N>().0 {
        if *entry == [0u8; N] {
            *entry = marker;
            changed = true;
        }
    }
    changed
}

/// The manager's free-cluster search scans whole FAT sectors without
/// re-checking its end bound mid-sector -- the FAT16 and FAT32 walks alike
/// -- so zeroed phantom entries after the last real cluster's entry in the
/// FAT's final entry-holding sector would be handed out as allocatable
/// clusters mapping past the end of the partition once the card fills up.
/// This firmware's FAT32 formatter marks those entries bad at format time;
/// foreign formatters commonly leave them zeroed, so mounting applies the
/// same marks to any FAT16/FAT32 card. Best-effort and idempotent: only
/// exactly-zero (free) phantom entries are rewritten -- pre-existing bad
/// marks or foreign garbage are left for a PC disk check -- and any
/// failure leaves the card as it was (a healthy or already marked card
/// costs one read per FAT copy and no writes).
fn repair_phantom_fat_tail<D: BlockDevice>(device: &D, layout: &MediaLayout) {
    let Some(boot) = read_device_sector(device, layout.start_lba) else {
        return;
    };
    let BootSector::Fat(geometry) = parse_fat_boot_sector(&boot) else {
        return;
    };
    let entry_bytes = fat_entry_bytes(geometry.cluster_count, geometry.fat32);
    let phantom_offset = (entry_bytes % 512) as usize;
    if phantom_offset == 0 {
        return;
    }
    // The sector holding the last real entry, located from the cluster
    // count rather than fat_sectors: a foreign formatter may over-provision
    // the FAT, and the manager's scan can only overrun within this sector
    // (its per-sector end-bound check stops it before any spare sector).
    // The parser's under-provision check uses the same fat_entry_bytes
    // expression to guarantee the declared FAT holds every entry, so with
    // phantom_offset != 0 this sector is strictly inside every FAT copy
    // and the cast cannot truncate.
    let last_entry_sector = (entry_bytes / 512) as u32;
    // The parser bounds the FAT span by the BPB's own total_sectors (so
    // the plain arithmetic cannot wrap), but `layout` came from an earlier
    // probe: a card swapped between probe and mount could still send the
    // writes below past the probed media, so the span is re-checked
    // against the probed size.
    let fat_span =
        u32::from(geometry.fat_copies) * geometry.fat_sectors + geometry.reserved_sectors;
    if fat_span > layout.sector_count {
        return;
    }
    for copy in 0..u32::from(geometry.fat_copies) {
        let lba = layout.start_lba
            + geometry.reserved_sectors
            + copy * geometry.fat_sectors
            + last_entry_sector;
        let Some(mut sector) = read_device_sector(device, lba) else {
            continue;
        };
        let changed = if geometry.fat32 {
            mark_phantom_tail(
                &mut sector[phantom_offset..],
                FAT32_BAD_CLUSTER.to_le_bytes(),
            )
        } else {
            // FAT16's 4,085-cluster floor keeps last_entry_sector >= 15, so
            // this is never the sector holding the reserved entries 0..2.
            mark_phantom_tail(
                &mut sector[phantom_offset..],
                FAT16_BAD_CLUSTER.to_le_bytes(),
            )
        };
        if changed {
            let _ = write_device_sector(device, lba, &sector);
        }
    }
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|component| !component.is_empty())
}

/// The last non-empty component of a path — the stored 8.3 short name of the
/// entry it addresses. The one definition shared by the library and the UI
/// (rename prefill, move destination), which must agree on it.
pub fn leaf_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|component| !component.is_empty())
        .unwrap_or("")
}

/// The manager's iteration surfaces the `.`/`..` dot entries and the volume
/// label; none of them are real children.
fn is_listable(entry: &DirEntry) -> bool {
    !(entry.attributes.is_volume()
        || entry.attributes.is_lfn()
        || entry.name == ShortFileName::this_dir()
        || entry.name == ShortFileName::parent_dir())
}

/// Provably an interrupted `make_dir`'s debris: a directory entry whose
/// on-disk first cluster is 0, which embedded-sdmmc remaps to
/// [`ClusterId::ROOT_DIR`] -- so opening it aliases the volume root, and
/// any walk through it (listing, recursion, the manager's own emptiness
/// check) reads the wrong directory. A real directory, even an empty one,
/// always owns the cluster holding its dot entries; only a `make_dir`
/// that failed between its entry write and its cluster assignment leaves
/// cluster 0. (The remap is gated on the directory attribute, so
/// zero-length files -- legitimately cluster 0 -- never match.)
fn entry_is_root_alias_debris(entry: &DirEntry) -> bool {
    entry.attributes.is_directory() && entry.cluster == ClusterId::ROOT_DIR
}

/// True when `path` lies strictly inside the directory `ancestor`.
/// Compared as bytes: paths may contain multi-byte characters (stored 8.3
/// high bytes re-rendered by ShortFileName's Display), so a str slice at
/// `ancestor.len()` could split a character and panic.
fn path_is_inside(ancestor: &str, path: &str) -> bool {
    let (ancestor, path) = (ancestor.as_bytes(), path.as_bytes());
    path.len() > ancestor.len()
        && path[..ancestor.len()].eq_ignore_ascii_case(ancestor)
        && path[ancestor.len()] == b'/'
}

/// The status wording for a move/rename whose source entry survived
/// ([`FsOpError::SourceRemains`]): shared between [`save_failure_reason`]
/// and the firmware's success-with-caveat explorer status so the two can
/// never drift apart.
pub const OLD_ENTRY_REMAINS: &str = "old entry remains";

/// Status-line prefixes shared between the firmware's status `format!`s
/// and the width tests, so the 32-column budget can never silently drift
/// when a message is reworded.
pub const SAVE_FAILED_PREFIX: &str = "Save failed: ";
pub const SAVE_FAILED_KEPT_PREFIX: &str = "Save failed; kept ";
pub const RENAME_FAILED_PREFIX: &str = "Rename failed: ";
pub const MOVE_FAILED_PREFIX: &str = "Move failed: ";
pub const NOT_DELETED_PREFIX: &str = "Not deleted: ";
pub const DELETE_INCOMPLETE_PREFIX: &str = "Delete incomplete: ";
pub const CREATE_FAILED_PREFIX: &str = "Create failed: ";

/// The whole status for a delete whose absence could not be confirmed
/// ([`FsOpError::VerifyFailed`] from a delete): the removal may in fact
/// have landed, so the wording asserts uncertainty, never non-deletion.
pub const DELETE_UNCONFIRMED: &str = "Delete unconfirmed; check list";

/// Reduce a filesystem/device error to a message that fits the Wio
/// Terminal's 32-column status line.
pub fn save_failure_reason<E: core::error::Error>(error: &FsOpError<E>) -> String {
    use embedded_sdmmc::Error;

    match error {
        FsOpError::Fs(Error::DeviceError(device)) => {
            compact_device_error(&alloc::format!("{device:?}"), "io")
        }
        FsOpError::Fs(Error::NotFound) => "file not found".into(),
        FsOpError::AlreadyExists
        | FsOpError::Fs(Error::FileAlreadyExists | Error::DirAlreadyExists) => {
            "file already exists".into()
        }
        FsOpError::NotADirectory | FsOpError::Fs(Error::OpenedFileAsDir) => "not a folder".into(),
        FsOpError::IsADirectory | FsOpError::Fs(Error::OpenedDirAsFile) => {
            "path is a folder".into()
        }
        FsOpError::MoveIntoSelf => "move into itself".into(),
        // Deliberately not "file already exists": nothing bearing the
        // destination name remains after the verified cleanup.
        FsOpError::CopyCollision => "copy name clash".into(),
        FsOpError::VerifyFailed => "verify failed".into(),
        FsOpError::TooDeep => "folders too deep".into(),
        FsOpError::DeviceUnresponsive => "SD not responding".into(),
        FsOpError::DeleteIncomplete(_) => "data removed".into(),
        // "Rename failed: partial copy left" is exactly 32 columns; the
        // leftover is the actionable fact, so it wins over the original
        // failure's wording.
        FsOpError::CleanupIncomplete(_) => "partial copy left".into(),
        // Success-with-caveat in the move UI (the destination holds the
        // verified copy); the same wording the explorer status uses via
        // [`OLD_ENTRY_REMAINS`].
        FsOpError::SourceRemains(_) => OLD_ENTRY_REMAINS.into(),
        FsOpError::Fs(Error::DeleteNonEmptyDir) => "folder is not empty".into(),
        FsOpError::Fs(Error::DiskFull | Error::NotEnoughSpace) => "storage is full".into(),
        // Its only producer is a device fault met right after a successful
        // cluster extension; calling that a full disk would be a lie.
        FsOpError::Fs(Error::AllocationError) => "SD card fault".into(),
        FsOpError::Fs(Error::ReadOnly) => "file is read-only".into(),
        FsOpError::Fs(Error::FilenameError(_) | Error::ConversionError) => {
            "invalid file name".into()
        }
        FsOpError::Fs(
            Error::FormatError(_)
            | Error::BadCluster
            | Error::UnterminatedFatChain
            | Error::EndOfFile
            | Error::InvalidOffset,
        ) => "filesystem is corrupt".into(),
        FsOpError::Fs(Error::Unsupported | Error::NoSuchVolume | Error::BadBlockSize(_)) => {
            "unsupported filesystem".into()
        }
        FsOpError::Fs(
            Error::TooManyOpenVolumes
            | Error::TooManyOpenDirs
            | Error::TooManyOpenFiles
            | Error::FileAlreadyOpen
            | Error::DirAlreadyOpen
            | Error::VolumeStillInUse
            | Error::VolumeAlreadyOpen
            | Error::BadHandle
            | Error::LockError,
        ) => "filesystem busy".into(),
    }
}

fn compact_device_error(detail: &str, operation: &str) -> String {
    match detail {
        "WriteError" => "card rejected write".into(),
        "ReadError" => "card rejected read".into(),
        "Transport" | "GpioError" => "SD SPI transport error".into(),
        "TimeoutWaitNotBusy" => "SD write timed out".into(),
        "TimeoutReadBuffer" => "SD read timed out".into(),
        "CardNotFound" => "SD card not found".into(),
        "BadState" => "SD card bad state".into(),
        "CantEnableCRC" => "SD CRC unavailable".into(),
        "Cmd58Error" | "RegisterReadError" => "SD register error".into(),
        _ if detail.starts_with("CrcError") => "SD CRC mismatch".into(),
        _ if detail.starts_with("TimeoutCommand") || detail.starts_with("TimeoutACommand") => {
            "SD command timed out".into()
        }
        _ => alloc::format!("SD {operation}: {detail}"),
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec;
    use core::cell::{Cell, RefCell};

    #[test]
    fn input_is_debounced_and_directions_repeat() {
        let mut input = InputEngine::new();
        let down = RawButtons::default().with(Button::Down, true);
        assert_eq!(input.update(down, 0), None);
        assert_eq!(input.update(down, 20), None);
        assert_eq!(input.update(down, 21), Some(Button::Down));
        assert_eq!(input.update(down, 430), None);
        assert_eq!(input.update(down, 431), Some(Button::Down));
    }

    #[test]
    fn repeat_stops_when_release_is_observed() {
        let mut input = InputEngine::new();
        let down = RawButtons::default().with(Button::Down, true);
        let up = RawButtons::default();
        assert_eq!(input.update(down, 0), None);
        assert_eq!(input.update(down, 21), Some(Button::Down));
        assert_eq!(input.update(down, 431), Some(Button::Down));
        // The raw release lands exactly on the next repeat deadline (533);
        // the repeat must not fire while the button is observably up, even
        // though the release is still inside the debounce window.
        assert_eq!(input.update(up, 533), None);
        assert_eq!(input.update(up, 554), None);
    }

    #[test]
    fn top_side_repeat_is_explicit_and_flagged() {
        let held = RawButtons::default().with(Button::TopLeft, true);
        let mut input = InputEngine::new();
        assert_eq!(input.update_event(held, 0, true), None);
        assert_eq!(
            input.update_event(held, 21, true),
            Some(ButtonEvent {
                button: Button::TopLeft,
                repeat: false
            })
        );
        assert_eq!(input.update_event(held, 430, true), None);
        assert_eq!(
            input.update_event(held, 431, true),
            Some(ButtonEvent {
                button: Button::TopLeft,
                repeat: true
            })
        );

        let mut disabled = InputEngine::new();
        disabled.update_event(held, 0, false);
        disabled.update_event(held, 21, false);
        assert_eq!(disabled.update_event(held, 431, false), None);
    }

    #[test]
    fn validates_names_and_extensions() {
        assert!(validate_file_stem("NOTES").is_ok());
        assert!(validate_file_stem("notes").is_ok());
        // Spaces were storable through long filenames; 8.3 names reject them.
        assert!(validate_file_stem("Trip notes (2)").is_err());
        assert!(validate_file_stem("bad:name").is_err());
        assert!(validate_file_stem("trailing.").is_err());
        assert!(validate_file_stem("TOOLONGNAME").is_err());
        assert!(validate_entry_name("Projects").is_ok());
        assert!(validate_entry_name("NOTES.TXT").is_ok());
        assert!(validate_entry_name("").is_err());
        assert!(validate_entry_name(".").is_err());
        assert!(validate_entry_name("..").is_err());
        assert!(validate_entry_name("bad/name").is_err());
        assert!(validate_entry_name("TOOLONGNAME.TXT").is_err());
        assert!(validate_entry_name("NOTES.TEXT").is_err());
        // ShortFileName would silently strip a bare trailing dot, storing a
        // different name than the one validated.
        assert_eq!(
            validate_entry_name("NOTES."),
            Err("Name cannot end with a dot")
        );
        assert_eq!(
            validate_entry_name("Dir."),
            Err("Name cannot end with a dot")
        );
        // ShortFileName also collapses repeated interior dots ("A..TXT"
        // parses to "A.TXT"); the round-trip guard refuses anything whose
        // stored form would differ from the typed name.
        assert_eq!(
            validate_entry_name("A..TXT"),
            Err("Name would be stored differently")
        );
        assert!(validate_entry_name(".TXT").is_err());
        assert!(validate_entry_name("A.B.TXT").is_err());
        // Case-only differences survive the round-trip: 8.3 names store
        // uppercase, and the keyboard types lowercase.
        assert!(validate_entry_name("notes.txt").is_ok());
        assert!(is_uwe_file("NOTES.uWe"));
        assert!(!is_uwe_file("notes.txt"));
        assert!(!is_uwe_file("notes.uweave"));
        assert_eq!(leaf_name("/A/B.TXT"), "B.TXT");
        assert_eq!(leaf_name("/A/"), "A");
        assert_eq!(leaf_name("/"), "");
    }

    #[test]
    fn geometry_reports_too_small_up_to_the_exact_minimum() {
        assert_eq!(fat32_geometry(68_628, 0), Err(FormatError::TooSmall));
        let geometry = fat32_geometry(68_629, 0).unwrap();
        assert_eq!(geometry.cluster_count, 65_525);
        assert_eq!(geometry.sectors_per_cluster, 1);
    }

    #[test]
    fn computes_valid_fat32_geometry() {
        let geometry = fat32_geometry(131_072, 0).unwrap();
        assert!(geometry.cluster_count >= 65_525);
        assert!(geometry.sectors_per_cluster.is_power_of_two());
        assert!(geometry.partition_start + geometry.partition_sectors <= 131_072);

        let reseeded = fat32_geometry(131_072, 7).unwrap();
        assert_ne!(geometry.volume_serial, reseeded.volume_serial);
        assert_eq!(
            fat32_geometry(131_072, 7).unwrap().volume_serial,
            reseeded.volume_serial
        );
    }

    struct MockBlocks {
        blocks: RefCell<Vec<Block>>,
    }

    impl MockBlocks {
        fn new(count: usize) -> Self {
            Self {
                blocks: RefCell::new(vec![Block::new(); count]),
            }
        }
    }

    impl BlockDevice for MockBlocks {
        type Error = RamError;

        fn read(&self, output: &mut [Block], start: BlockIdx) -> Result<(), Self::Error> {
            let blocks = self.blocks.borrow();
            output.clone_from_slice(&blocks[start.0 as usize..start.0 as usize + output.len()]);
            Ok(())
        }

        fn write(&self, input: &[Block], start: BlockIdx) -> Result<(), Self::Error> {
            let mut blocks = self.blocks.borrow_mut();
            blocks[start.0 as usize..start.0 as usize + input.len()].clone_from_slice(input);
            Ok(())
        }

        fn num_blocks(&self) -> Result<BlockCount, Self::Error> {
            Ok(BlockCount(self.blocks.borrow().len() as u32))
        }
    }

    #[test]
    fn sd_stream_handles_partial_cross_sector_io() {
        let mut stream = SdStream::new(MockBlocks::new(3)).unwrap();
        stream.seek(SeekFrom::Start(510)).unwrap();
        stream.write_all(b"abcdef").unwrap();
        stream.seek(SeekFrom::Start(510)).unwrap();
        let mut result = [0; 6];
        assert_eq!(stream.read(&mut result).unwrap(), 6);
        assert_eq!(&result, b"abcdef");
        assert_eq!(
            stream.seek(SeekFrom::End(1)),
            Err(SdStreamError::OutOfBounds)
        );
    }

    struct RamDisk {
        bytes: Vec<u8>,
        position: u64,
        fail_writes: Rc<Cell<bool>>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RamError;

    impl fmt::Display for RamError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("RAM disk error")
        }
    }
    impl core::error::Error for RamError {}
    impl embedded_io::Error for RamError {
        fn kind(&self) -> embedded_io::ErrorKind {
            embedded_io::ErrorKind::Other
        }
    }
    impl ErrorType for RamDisk {
        type Error = RamError;
    }
    impl Read for RamDisk {
        fn read(&mut self, out: &mut [u8]) -> Result<usize, RamError> {
            let start = self.position as usize;
            let count = out.len().min(self.bytes.len().saturating_sub(start));
            out[..count].copy_from_slice(&self.bytes[start..start + count]);
            self.position += count as u64;
            Ok(count)
        }
    }
    impl Write for RamDisk {
        fn write(&mut self, input: &[u8]) -> Result<usize, RamError> {
            if self.fail_writes.get() {
                return Err(RamError);
            }
            let start = self.position as usize;
            if start + input.len() > self.bytes.len() {
                return Err(RamError);
            }
            self.bytes[start..start + input.len()].copy_from_slice(input);
            self.position += input.len() as u64;
            Ok(input.len())
        }
        fn flush(&mut self) -> Result<(), RamError> {
            Ok(())
        }
    }
    impl Seek for RamDisk {
        fn seek(&mut self, from: SeekFrom) -> Result<u64, RamError> {
            let next = match from {
                SeekFrom::Start(n) => n as i64,
                SeekFrom::Current(n) => self.position as i64 + n,
                SeekFrom::End(n) => self.bytes.len() as i64 + n,
            };
            if next < 0 || next as usize > self.bytes.len() {
                return Err(RamError);
            }
            self.position = next as u64;
            Ok(self.position)
        }
    }

    #[derive(Clone)]
    struct RamBlocks {
        blocks: Rc<RefCell<Vec<Block>>>,
        fail_writes: Rc<Cell<bool>>,
        /// After this many successful writes, silently flip one byte of the
        /// next write (then behave normally). Downstream this surfaces as
        /// `VerifyFailed` -- a non-device error -- unlike `fail_writes`'
        /// hard device fault, which is the distinction the save/move cleanup
        /// gating turns on.
        corrupt_one_write_after: Rc<Cell<Option<u32>>>,
        /// After this many successful writes, latch `fail_writes`: a card
        /// that dies mid-operation and stays dead.
        fail_writes_after: Rc<Cell<Option<u32>>>,
        /// After this many write calls, fail exactly one write, then behave
        /// normally: a transient write flake.
        fail_one_write_after: Rc<Cell<Option<u32>>>,
        /// After this many write calls the card dies completely: every later
        /// read AND write fails until `fail_writes`/`fail_reads` are cleared.
        die_after_writes: Rc<Cell<Option<u32>>>,
        /// Latched total read failure (a dead card).
        fail_reads: Rc<Cell<bool>>,
        writes_seen: Rc<Cell<u32>>,
        reads_seen: Rc<Cell<u32>>,
        /// Fail this many read calls, then behave normally: the transient
        /// per-command misses real cards produce.
        fail_next_reads: Rc<Cell<u32>>,
        /// After this many read calls, fail exactly one read, then behave
        /// normally: a transient read flake at a chosen position.
        fail_one_read_after: Rc<Cell<Option<u32>>>,
        /// Companion to `fail_one_read_after`: when it fires, queue this
        /// many further consecutive read failures (via `fail_next_reads`),
        /// stretching the positioned one-shot flake into a positioned
        /// outage long enough to outlast every retry layer.
        fail_one_read_burst: Rc<Cell<u32>>,
        /// Fail every read whose block range intersects `[start, end)`: a
        /// region (e.g. the whole FAT) that stops answering. The manager's
        /// directory walks launder such chain-read failures into NotFound
        /// on *every* walk -- the correlated double-laundered residue the
        /// single-flake knobs cannot produce.
        fail_reads_in: Rc<Cell<Option<(u32, u32)>>>,
        /// Failure budget for `fail_reads_in`: while set, each failure the
        /// region produces decrements it, and at zero both knobs clear --
        /// a region that stops answering and then heals, which a
        /// double-laundered scan miss followed by an honest later walk
        /// requires. `None` keeps the region dead for good.
        fail_reads_in_budget: Rc<Cell<Option<u32>>>,
    }

    impl RamBlocks {
        fn new(sectors: u32) -> Self {
            Self {
                blocks: Rc::new(RefCell::new(vec![Block::new(); sectors as usize])),
                fail_writes: Rc::new(Cell::new(false)),
                corrupt_one_write_after: Rc::new(Cell::new(None)),
                fail_writes_after: Rc::new(Cell::new(None)),
                fail_one_write_after: Rc::new(Cell::new(None)),
                die_after_writes: Rc::new(Cell::new(None)),
                fail_reads: Rc::new(Cell::new(false)),
                writes_seen: Rc::new(Cell::new(0)),
                reads_seen: Rc::new(Cell::new(0)),
                fail_next_reads: Rc::new(Cell::new(0)),
                fail_one_read_after: Rc::new(Cell::new(None)),
                fail_one_read_burst: Rc::new(Cell::new(0)),
                fail_reads_in: Rc::new(Cell::new(None)),
                fail_reads_in_budget: Rc::new(Cell::new(None)),
            }
        }
    }

    /// Advance a fault knob's countdown by one call: `true` exactly when the
    /// countdown expires on this call (the knob then clears so it fires
    /// once). Every positioned knob shares this decrement so an off-by-one
    /// fix lands in all of them at once.
    fn countdown_fired(cell: &Cell<Option<u32>>) -> bool {
        match cell.get() {
            Some(0) => {
                cell.set(None);
                true
            }
            Some(remaining) => {
                cell.set(Some(remaining - 1));
                false
            }
            None => false,
        }
    }

    impl BlockDevice for RamBlocks {
        type Error = RamError;

        fn read(&self, output: &mut [Block], start: BlockIdx) -> Result<(), RamError> {
            if self.fail_reads.get() {
                return Err(RamError);
            }
            let failures = self.fail_next_reads.get();
            if failures > 0 {
                self.fail_next_reads.set(failures - 1);
                return Err(RamError);
            }
            if countdown_fired(&self.fail_one_read_after) {
                let burst = self.fail_one_read_burst.get();
                if burst > 0 {
                    self.fail_next_reads.set(self.fail_next_reads.get() + burst);
                }
                return Err(RamError);
            }
            if let Some((region_start, region_end)) = self.fail_reads_in.get() {
                let first = start.0;
                if first < region_end && first + output.len() as u32 > region_start {
                    match self.fail_reads_in_budget.get() {
                        // Budget spent: the region heals and this read is
                        // served normally.
                        Some(0) => {
                            self.fail_reads_in.set(None);
                            self.fail_reads_in_budget.set(None);
                        }
                        Some(remaining) => {
                            self.fail_reads_in_budget.set(Some(remaining - 1));
                            return Err(RamError);
                        }
                        None => return Err(RamError),
                    }
                }
            }
            let blocks = self.blocks.borrow();
            let start = start.0 as usize;
            let end = start + output.len();
            if end > blocks.len() {
                return Err(RamError);
            }
            output.clone_from_slice(&blocks[start..end]);
            self.reads_seen.set(self.reads_seen.get() + 1);
            Ok(())
        }

        fn write(&self, input: &[Block], start: BlockIdx) -> Result<(), RamError> {
            // die_after_writes latches before fail_writes_after so a test
            // may arm both; the gate below then sees either latch.
            if countdown_fired(&self.die_after_writes) {
                self.fail_writes.set(true);
                self.fail_reads.set(true);
            }
            if countdown_fired(&self.fail_writes_after) {
                self.fail_writes.set(true);
            }
            if self.fail_writes.get() {
                return Err(RamError);
            }
            if countdown_fired(&self.fail_one_write_after) {
                return Err(RamError);
            }
            let mut blocks = self.blocks.borrow_mut();
            let start = start.0 as usize;
            let end = start + input.len();
            if end > blocks.len() {
                return Err(RamError);
            }
            blocks[start..end].clone_from_slice(input);
            if countdown_fired(&self.corrupt_one_write_after) {
                blocks[start].contents[0] ^= 0xff;
            }
            self.writes_seen.set(self.writes_seen.get() + 1);
            Ok(())
        }

        fn num_blocks(&self) -> Result<BlockCount, RamError> {
            Ok(BlockCount(self.blocks.borrow().len() as u32))
        }
    }

    /// Device-level retry parity with the firmware's controller device
    /// (recovery is a no-op for RAM). Used only where a test injects the
    /// transient faults the library itself no longer retries.
    #[derive(Clone)]
    struct RetryBlocks(RamBlocks);

    impl BlockDevice for RetryBlocks {
        type Error = RamError;

        fn read(&self, output: &mut [Block], start: BlockIdx) -> Result<(), RamError> {
            sd_retry(|| self.0.read(output, start))
        }

        fn write(&self, input: &[Block], start: BlockIdx) -> Result<(), RamError> {
            sd_retry(|| self.0.write(input, start))
        }

        fn num_blocks(&self) -> Result<BlockCount, RamError> {
            sd_retry(|| self.0.num_blocks())
        }
    }

    /// A freshly formatted 64 MiB volume and a CardFs mounted on it.
    fn formatted_fs(sectors: u32) -> (RamBlocks, CardFs<RamBlocks>) {
        let ram = RamBlocks::new(sectors);
        format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        (ram, fs)
    }

    /// Root-directory entry names beginning with `~WIO` (staging leftovers).
    fn staging_leftovers(fs: &CardFs<RamBlocks>) -> Vec<String> {
        let (page, _) = fs.read_directory_page("/", 0, 8).unwrap();
        page.into_iter()
            .map(|item| item.name)
            .filter(|name| name.starts_with("~WIO"))
            .collect()
    }

    /// Writes consumed by a save's staging steps (TMP write + BAK copy),
    /// measured on a twin fixture with an identical write history, so
    /// injection tests can aim at the commit step deterministically.
    fn staging_write_count(new_contents: &[u8], original: &[u8]) -> u32 {
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/FILE.TXT", original).unwrap();
        let before = ram.writes_seen.get();
        fs.save_verified("/~WIO0000.TMP", new_contents).unwrap();
        fs.copy_file_contents(
            "/FILE.TXT",
            "/~WIO0000.BAK",
            Mode::ReadWriteCreate,
            &mut false,
        )
        .unwrap();
        ram.writes_seen.get() - before
    }

    #[test]
    fn formatted_media_mounts_and_round_trips() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let fail_writes = ram.fail_writes.clone();
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        assert_eq!(layout.start_lba, geometry.partition_start);
        assert_eq!(layout.partition_index, 0);
        assert!(layout.fat32);
        let fs = CardFs::mount(ram.clone(), layout).unwrap();

        fs.save_verified("/FIRST.TXT", b"hello").unwrap();
        assert_eq!(fs.read_file("/FIRST.TXT").unwrap(), b"hello");
        fs.save_transactional("/FIRST.TXT", b"replacement").unwrap();
        assert_eq!(fs.read_file("/FIRST.TXT").unwrap(), b"replacement");
        fs.save_transactional("/BRANDNEW.TXT", b"created by save")
            .unwrap();
        assert_eq!(fs.read_file("/BRANDNEW.TXT").unwrap(), b"created by save");
        fs.create_empty("/EMPTY.TXT").unwrap();
        fs.save_verified("/EMPTY.TXT", b"first contents").unwrap();
        assert!(matches!(
            fs.create_empty("/FIRST.TXT"),
            Err(FsOpError::AlreadyExists)
        ));

        let (page, total) = fs.read_directory_page("/", 0, 8).unwrap();
        assert_eq!(total, 3);
        let names: Vec<String> = page.iter().map(|item| item.name.clone()).collect();
        // EMPTY.TXT reused the directory slot freed by BRANDNEW.TXT's
        // deleted ~WIO0000.TMP staging file, so it lists second.
        assert_eq!(
            names,
            vec![
                String::from("FIRST.TXT"),
                String::from("EMPTY.TXT"),
                String::from("BRANDNEW.TXT")
            ]
        );
        assert_eq!(page[0].path, "/FIRST.TXT");
        let (rest, _) = fs.read_directory_page("/", 2, 8).unwrap();
        assert_eq!(rest[0].name, "BRANDNEW.TXT");
        assert_eq!(fs.entry_index("/", "/EMPTY.TXT").unwrap(), Some(1));

        fs.create_directory_verified("/PROJECTS").unwrap();
        fs.create_directory_verified("/ARCHIVE").unwrap();
        fs.create_directory_verified("/PROJECTS/NESTED").unwrap();
        fs.create_empty("/PROJECTS/NESTED/README.TXT").unwrap();
        assert!(matches!(
            fs.create_directory_verified("/PROJECTS"),
            Err(FsOpError::AlreadyExists)
        ));

        let (folders, folder_total) = fs
            .read_directory_folders_page("/", 0, 8, Some("/ARCHIVE"))
            .unwrap();
        assert_eq!(folder_total, 1);
        assert_eq!(folders[0].name, "PROJECTS");

        fs.move_entry_verified("/EMPTY.TXT", "/RENAMED.TXT")
            .unwrap();
        assert_eq!(fs.read_file("/RENAMED.TXT").unwrap(), b"first contents");
        assert!(!fs.entry_exists("/EMPTY.TXT").unwrap());
        fs.move_entry_verified("/RENAMED.TXT", "/PROJECTS/RENAMED.TXT")
            .unwrap();
        assert_eq!(
            fs.read_file("/PROJECTS/RENAMED.TXT").unwrap(),
            b"first contents"
        );
        assert!(matches!(
            fs.move_entry_verified("/FIRST.TXT", "/PROJECTS/RENAMED.TXT"),
            Err(FsOpError::AlreadyExists)
        ));
        assert!(matches!(
            fs.move_entry_verified("/PROJECTS", "/PROJECTS/NESTED/LOOPED"),
            Err(FsOpError::MoveIntoSelf)
        ));
        fs.move_entry_verified("/PROJECTS/NESTED", "/ARCHIVE/NESTED")
            .unwrap();
        assert_eq!(fs.read_file("/ARCHIVE/NESTED/README.TXT").unwrap(), b"");
        assert!(!fs.entry_exists("/PROJECTS/NESTED").unwrap());

        fs.delete_verified("/FIRST.TXT", false).unwrap();
        fs.delete_verified("/PROJECTS", true).unwrap();
        fs.delete_verified("/ARCHIVE", true).unwrap();
        assert!(!fs.entry_exists("/PROJECTS").unwrap());
        assert!(!fs.entry_exists("/ARCHIVE").unwrap());

        fs.create_empty("/DURABLE.TXT").unwrap();
        fs.save_verified("/DURABLE.TXT", b"old contents").unwrap();

        // A failed write must never be reported as success, even though the
        // manager could serve the intended state from its block cache. The
        // staged temporary write fails before the target is touched.
        fail_writes.set(true);
        assert!(matches!(
            fs.save_transactional("/DURABLE.TXT", b"new contents"),
            Err(ref error) if error.error.is_device_error()
        ));
        assert!(matches!(
            fs.create_directory_verified("/MUSTFAIL"),
            Err(ref error) if error.is_device_error()
        ));
        assert!(matches!(
            fs.create_empty("/ALSOFAIL.TXT"),
            Err(ref error) if error.is_device_error()
        ));
        fail_writes.set(false);
        assert_eq!(fs.read_file("/DURABLE.TXT").unwrap(), b"old contents");

        // Remount and confirm the old contents actually reached the "card"
        // rather than surviving only in the manager's cache.
        drop(fs);
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(fs.read_file("/DURABLE.TXT").unwrap(), b"old contents");
    }

    #[test]
    fn long_names_are_listed_and_open_via_short_alias() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();

        // Hand-write an LFN fragment + short-name pair into the empty root
        // directory: embedded-sdmmc reads long names but can never create
        // them, so the fixture builds what a PC would have written.
        let short = ShortFileName::create_from_str("ALONGF~1.TXT").unwrap();
        let long_name: Vec<u16> = "A long nm.txt".encode_utf16().collect();
        assert_eq!(long_name.len(), 13);
        let mut entries = [0u8; 64];
        entries[0] = 0x41; // sequence 1, end of chain
        entries[11] = 0x0f; // long-filename attribute
        entries[13] = short.csum();
        let unit_offsets: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (unit, offset) in long_name.iter().zip(unit_offsets) {
            entries[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
        }
        entries[32..43].copy_from_slice(b"ALONGF~1TXT");
        entries[43] = 0x20; // archive attribute; cluster and size stay zero
        let root_lba = geometry.partition_start + 32 + 2 * geometry.fat_sectors;
        ram.blocks.borrow_mut()[root_lba as usize].contents[..64].copy_from_slice(&entries);

        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        let (page, total) = fs.read_directory_page("/", 0, 8).unwrap();
        assert_eq!(total, 1);
        assert_eq!(page[0].name, "A long nm.txt");
        assert_eq!(page[0].path, "/ALONGF~1.TXT");
        assert!(fs.read_file("/ALONGF~1.TXT").unwrap().is_empty());
    }

    #[test]
    fn format_erases_stale_gpt_headers() {
        let sectors = 131_072u32;
        let mut disk = RamDisk {
            bytes: vec![0; sectors as usize * 512],
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        let last = (sectors as usize - 1) * 512;
        disk.bytes[512..520].copy_from_slice(b"EFI PART");
        disk.bytes[last..last + 8].copy_from_slice(b"EFI PART");

        format_fat32(&mut disk, sectors, 0).unwrap();

        assert!(disk.bytes[512..1024].iter().all(|&byte| byte == 0));
        assert!(disk.bytes[last..last + 512].iter().all(|&byte| byte == 0));
        disk.position = 0;
        assert!(probe_fat(&mut disk, sectors).is_ok());
    }

    #[test]
    fn probe_rejects_bpb_larger_than_its_container() {
        let geometry = FormatGeometry {
            partition_start: 0,
            partition_sectors: 100_000,
            sectors_per_cluster: 1,
            fat_sectors: 800,
            cluster_count: 98_368,
            volume_serial: 1,
        };

        let mut superfloppy = RamDisk {
            bytes: make_boot_sector(&geometry).to_vec(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(
            probe_fat(&mut superfloppy, geometry.partition_sectors - 1),
            Err(ProbeError::Invalid)
        );

        let partition_start = 1u32;
        let partition_sectors = geometry.partition_sectors;
        let mut mbr = vec![0u8; 1024];
        mbr[446 + 4] = 0x0c;
        put_u32(&mut mbr[446 + 8..446 + 12], partition_start);
        put_u32(&mut mbr[446 + 12..446 + 16], partition_sectors);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut oversized_boot = make_boot_sector(&FormatGeometry {
            partition_start,
            partition_sectors: partition_sectors + 1,
            ..geometry
        });
        put_u32(&mut oversized_boot[32..36], partition_sectors + 1);
        mbr[512..1024].copy_from_slice(&oversized_boot);
        let mut partitioned = RamDisk {
            bytes: mbr,
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(
            probe_fat(&mut partitioned, partition_start + partition_sectors),
            Err(ProbeError::Invalid)
        );
    }

    #[test]
    fn probe_rejects_small_fat32_as_unsupported() {
        // Structurally FAT32 (root_entries = 0, fat16 = 0, fat32 set, root
        // cluster at offset 44) but with fewer than 65,525 clusters, as
        // mkfs.vfat -F 32 produces on small media. embedded-sdmmc classifies
        // FAT width by cluster count alone and would mount this as FAT16,
        // misreading the volume, so the probe must refuse it as unsupported
        // rather than let it reach the mount.
        let mut sector = [0u8; 512];
        sector[11..13].copy_from_slice(&512u16.to_le_bytes());
        sector[13] = 1;
        sector[14..16].copy_from_slice(&32u16.to_le_bytes());
        sector[16] = 2;
        sector[32..36].copy_from_slice(&40_000u32.to_le_bytes());
        sector[36..40].copy_from_slice(&320u32.to_le_bytes());
        sector[44..48].copy_from_slice(&2u32.to_le_bytes());
        sector[66] = 0x29;
        sector[67..71].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        sector[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut disk = RamDisk {
            bytes: sector.to_vec(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(probe_fat(&mut disk, 40_000), Err(ProbeError::Unsupported));
    }

    #[test]
    fn probe_reports_fat16_layout_and_serial() {
        // A FAT16-layout BPB (root_entries and 16-bit FAT size set): the
        // serial lives at offset 39 and the layout must report fat32 = false
        // so delete-time cluster reclaim stays disabled.
        let mut sector = [0u8; 512];
        sector[11..13].copy_from_slice(&512u16.to_le_bytes());
        sector[13] = 1;
        sector[14..16].copy_from_slice(&1u16.to_le_bytes());
        sector[16] = 2;
        sector[17..19].copy_from_slice(&512u16.to_le_bytes());
        sector[19..21].copy_from_slice(&40_000u16.to_le_bytes());
        sector[22..24].copy_from_slice(&160u16.to_le_bytes());
        sector[39..43].copy_from_slice(&0xfeed_f00du32.to_le_bytes());
        sector[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut disk = RamDisk {
            bytes: sector.to_vec(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        let layout = probe_fat(&mut disk, 40_000).unwrap();
        assert_eq!(layout.volume_serial, 0xfeed_f00d);
        assert_eq!(layout.sector_count, 40_000);
        assert!(!layout.fat32);
    }

    #[test]
    fn probe_classifies_exfat_and_fat12_as_unsupported() {
        let mut exfat = [0u8; 512];
        exfat[0..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
        exfat[3..11].copy_from_slice(b"EXFAT   ");
        exfat[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut disk = RamDisk {
            bytes: exfat.to_vec(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(
            probe_fat(&mut disk, 1_000_000),
            Err(ProbeError::Unsupported)
        );

        // A FAT16-layout BPB whose cluster count lands in FAT12 territory.
        let mut fat12 = [0u8; 512];
        fat12[11..13].copy_from_slice(&512u16.to_le_bytes());
        fat12[13] = 1;
        fat12[14..16].copy_from_slice(&1u16.to_le_bytes());
        fat12[16] = 2;
        fat12[17..19].copy_from_slice(&512u16.to_le_bytes());
        fat12[19..21].copy_from_slice(&2_000u16.to_le_bytes());
        fat12[22..24].copy_from_slice(&8u16.to_le_bytes());
        fat12[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut disk = RamDisk {
            bytes: fat12.to_vec(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(probe_fat(&mut disk, 2_000), Err(ProbeError::Unsupported));
    }

    #[test]
    fn probe_skips_damaged_partition_and_finds_later_fat() {
        let geometry = FormatGeometry {
            partition_start: 2_048,
            partition_sectors: 100_000,
            sectors_per_cluster: 1,
            fat_sectors: 800,
            cluster_count: 98_368,
            volume_serial: 0x1234_5678,
        };
        let media = 2_048 + 100_000;
        // Entry 0 is FAT32-typed but points at zeroed sectors; entry 1 holds
        // the healthy partition. The probe must skip the damaged entry
        // instead of failing the whole card.
        let mut bytes = vec![0u8; (2_048 + 1) * 512];
        bytes[446 + 4] = 0x0c;
        put_u32(&mut bytes[446 + 8..446 + 12], 1);
        put_u32(&mut bytes[446 + 12..446 + 16], 100);
        bytes[462 + 4] = 0x0c;
        put_u32(&mut bytes[462 + 8..462 + 12], 2_048);
        put_u32(&mut bytes[462 + 12..462 + 16], 100_000);
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        let boot = make_boot_sector(&geometry);
        bytes[2_048 * 512..2_049 * 512].copy_from_slice(&boot);
        let mut disk = RamDisk {
            bytes,
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        let layout = probe_fat(&mut disk, media).unwrap();
        assert_eq!(layout.start_lba, 2_048);
        assert_eq!(layout.sector_count, 100_000);
        assert_eq!(layout.volume_serial, 0x1234_5678);
        assert!(layout.fat32);
        // The mount must open the same MBR slot the probe accepted, not
        // blindly slot 0.
        assert_eq!(layout.partition_index, 1);
    }

    #[test]
    fn probe_skips_status_flagged_partition_slot() {
        let geometry = FormatGeometry {
            partition_start: 2_048,
            partition_sectors: 100_000,
            sectors_per_cluster: 1,
            fat_sectors: 800,
            cluster_count: 98_368,
            volume_serial: 0x1234_5678,
        };
        let media = 2_048 + 100_000;
        // Both slots point at the same healthy volume, but slot 0 carries a
        // status byte the mount layer refuses (any bit besides 0x80): the
        // probe must skip it, and must still accept slot 1's legal bootable
        // flag 0x80.
        let mut bytes = vec![0u8; (2_048 + 1) * 512];
        bytes[446] = 0x01;
        bytes[446 + 4] = 0x0c;
        put_u32(&mut bytes[446 + 8..446 + 12], 2_048);
        put_u32(&mut bytes[446 + 12..446 + 16], 100_000);
        bytes[462] = 0x80;
        bytes[462 + 4] = 0x0c;
        put_u32(&mut bytes[462 + 8..462 + 12], 2_048);
        put_u32(&mut bytes[462 + 12..462 + 16], 100_000);
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        let boot = make_boot_sector(&geometry);
        bytes[2_048 * 512..2_049 * 512].copy_from_slice(&boot);
        let mut disk = RamDisk {
            bytes: bytes.clone(),
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        let layout = probe_fat(&mut disk, media).unwrap();
        assert_eq!(layout.partition_index, 1);
        assert_eq!(layout.volume_serial, 0x1234_5678);

        // With only the flagged slot present the probe finds nothing.
        bytes[462..478].fill(0);
        let mut disk = RamDisk {
            bytes,
            position: 0,
            fail_writes: Rc::new(Cell::new(false)),
        };
        assert_eq!(probe_fat(&mut disk, media), Err(ProbeError::Invalid));
    }

    /// Debug-formats like `embedded_sdmmc::SdCardError::WriteError` so the
    /// compact-message mapping can be tested without a real card.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FakeSdError;

    impl fmt::Debug for FakeSdError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("WriteError")
        }
    }

    impl fmt::Display for FakeSdError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("WriteError")
        }
    }

    impl core::error::Error for FakeSdError {}

    #[test]
    fn save_errors_are_compact_enough_for_the_status_line() {
        // Each status is built from the shared prefix constant the firmware
        // itself uses and pinned against the full literal, so both the
        // wording and the 32-column budget are guarded in one place.
        let error: FsOpError<FakeSdError> =
            FsOpError::Fs(embedded_sdmmc::Error::DeviceError(FakeSdError));
        let status = alloc::format!("{SAVE_FAILED_PREFIX}{}", save_failure_reason(&error));
        assert_eq!(status, "Save failed: card rejected write");
        assert!(status.chars().count() <= 32);
        assert_eq!(
            save_failure_reason::<FakeSdError>(&FsOpError::AlreadyExists),
            "file already exists"
        );
        assert_eq!(
            save_failure_reason::<FakeSdError>(&FsOpError::Fs(embedded_sdmmc::Error::DiskFull)),
            "storage is full"
        );
        assert_eq!(
            save_failure_reason::<FakeSdError>(&FsOpError::VerifyFailed),
            "verify failed"
        );
        assert_eq!(
            save_failure_reason::<FakeSdError>(&FsOpError::Fs(
                embedded_sdmmc::Error::AllocationError
            )),
            "SD card fault"
        );
        let unresponsive = alloc::format!(
            "{RENAME_FAILED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::DeviceUnresponsive)
        );
        assert_eq!(unresponsive, "Rename failed: SD not responding");
        assert!(unresponsive.chars().count() <= 32);
        let emptied = alloc::format!(
            "{DELETE_INCOMPLETE_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::DeleteIncomplete(Box::new(
                FsOpError::Fs(embedded_sdmmc::Error::DeviceError(FakeSdError))
            )))
        );
        assert_eq!(emptied, "Delete incomplete: data removed");
        assert!(emptied.chars().count() <= 32);
        let ghost = alloc::format!(
            "{RENAME_FAILED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::CleanupIncomplete(Box::new(
                FsOpError::VerifyFailed
            )))
        );
        assert_eq!(ghost, "Rename failed: partial copy left");
        assert!(ghost.chars().count() <= 32);
        // A child collision after a completed cleanup: nothing bearing the
        // destination name remains, so the wording must not claim an
        // existing file.
        let collision = alloc::format!(
            "{RENAME_FAILED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::CopyCollision)
        );
        assert_eq!(collision, "Rename failed: copy name clash");
        assert!(collision.chars().count() <= 32);
        // SourceRemains lands on the success-with-caveat path in the UI;
        // this guards the wording if a failure banner ever shows it.
        let source_remains = alloc::format!(
            "{MOVE_FAILED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::SourceRemains(Box::new(
                FsOpError::VerifyFailed
            )))
        );
        assert_eq!(source_remains, "Move failed: old entry remains");
        assert!(source_remains.chars().count() <= 32);
        // The delete prompt's non-DeleteIncomplete prefix with its longest
        // common reason is exactly the 32-column budget.
        let not_deleted = alloc::format!(
            "{NOT_DELETED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::Fs(
                embedded_sdmmc::Error::DeleteNonEmptyDir
            ))
        );
        assert_eq!(not_deleted, "Not deleted: folder is not empty");
        assert!(not_deleted.chars().count() <= 32);
        let read_only = alloc::format!(
            "{NOT_DELETED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::Fs(embedded_sdmmc::Error::ReadOnly))
        );
        assert_eq!(read_only, "Not deleted: file is read-only");
        assert!(read_only.chars().count() <= 32);
        // The create prompt's prefix with its longest reason is exactly the
        // 32-column budget.
        let create_failed = alloc::format!(
            "{CREATE_FAILED_PREFIX}{}",
            save_failure_reason::<FakeSdError>(&FsOpError::DeviceUnresponsive)
        );
        assert_eq!(create_failed, "Create failed: SD not responding");
        assert!(create_failed.chars().count() <= 32);
        // The unconfirmable-delete status is a whole string, not a prefix.
        assert_eq!(DELETE_UNCONFIRMED, "Delete unconfirmed; check list");
        assert!(DELETE_UNCONFIRMED.chars().count() <= 32);
        // The exit dialog repeats the editor status in a 30-column box, and
        // "kept" messages must survive it unclipped.
        let kept = alloc::format!("{SAVE_FAILED_KEPT_PREFIX}~WIO0000.BAK");
        assert_eq!(kept, "Save failed; kept ~WIO0000.BAK");
        assert!(kept.chars().count() <= 30);
    }

    #[test]
    fn transactional_save_has_no_editor_size_rejection() {
        let (_ram, fs) = formatted_fs(131_072);
        // Universal Weave documents use available RAM/filesystem capacity as
        // their limit; the transactional layer must not retain the former
        // text editor's 32 KiB policy.
        let oversized = vec![0x41u8; 32 * 1024 + 1];
        fs.save_verified("/GROWN.TXT", &oversized).unwrap();
        fs.save_transactional("/GROWN.TXT", b"small").unwrap();
        assert_eq!(fs.read_file("/GROWN.TXT").unwrap(), b"small");
        assert!(staging_leftovers(&fs).is_empty());
    }

    #[test]
    fn create_new_save_refuses_collision_and_reads_back_aligned() {
        let (_ram, fs) = formatted_fs(131_072);
        fs.save_verified("/NEW.UWE", b"preexisting").unwrap();
        assert!(matches!(
            fs.save_transactional_create_new("/NEW.UWE", b"replacement"),
            Err(SaveError {
                error: FsOpError::AlreadyExists,
                ..
            })
        ));
        assert_eq!(fs.read_file("/NEW.UWE").unwrap(), b"preexisting");

        let contents = vec![0x5au8; 33 * 1024];
        fs.save_transactional_create_new("/FRESH.UWE", &contents)
            .unwrap();
        let aligned = fs.read_file_aligned("/FRESH.UWE").unwrap();
        assert_eq!(aligned.as_ptr() as usize % 16, 0);
        assert_eq!(aligned.as_slice(), contents);
    }

    #[test]
    fn save_rolls_back_from_backup_when_commit_verify_fails() {
        let original = vec![0x55u8; 2048];
        let new_contents = vec![0xaau8; 2048];
        let staging = staging_write_count(&new_contents, &original);
        let mut rollback_proven = false;
        // Sweep a single-write corruption across every write of the commit
        // and cleanup steps. Data-write hits fail the commit's read-back
        // verification and must leave the target restored from the on-card
        // backup with no staging leftovers; whichever writes the sweep lands
        // on, the loop ends with the uncorrupted control run succeeding.
        for extra in 0.. {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", &original).unwrap();
            ram.corrupt_one_write_after.set(Some(staging + extra));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            if ram.corrupt_one_write_after.get().is_some() {
                // The injection point lies beyond the whole save: control run.
                assert!(result.is_ok());
                break;
            }
            if matches!(
                result,
                Err(SaveError {
                    error: FsOpError::VerifyFailed,
                    ..
                })
            ) && fs
                .read_file("/FILE.TXT")
                .is_ok_and(|bytes| bytes == original)
                && staging_leftovers(&fs).is_empty()
            {
                rollback_proven = true;
            }
        }
        assert!(rollback_proven);
    }

    #[test]
    fn save_leaves_staging_files_when_the_card_dies_at_commit() {
        let original = vec![0x55u8; 2048];
        let new_contents = vec![0xaau8; 2048];
        let staging = staging_write_count(&new_contents, &original);
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/FILE.TXT", &original).unwrap();
        // The first commit write hits a card that stays dead: after a device
        // error no further mutations are trustworthy, so both staging copies
        // must survive as the recovery data the doc comment promises.
        ram.fail_writes_after.set(Some(staging));
        let result = fs.save_transactional("/FILE.TXT", &new_contents);
        let error = result.expect_err("the dead card cannot commit");
        assert!(error.error.is_device_error());
        // The headline of the SaveError plumbing: the caller learns the
        // surviving backup's name so the user can be pointed at it.
        assert_eq!(error.backup_kept.as_deref(), Some("/~WIO0000.BAK"));
        ram.fail_writes.set(false);
        assert_eq!(fs.read_file("/~WIO0000.TMP").unwrap(), new_contents);
        assert_eq!(fs.read_file("/~WIO0000.BAK").unwrap(), original);
    }

    /// Phase 1 of every kept-backup sweep: the corruption position (counted
    /// past `staging` writes) that makes the commit's verify fail and the
    /// rollback restore the target -- corruption of rewritten or unverified
    /// metadata blocks does neither, so the position must be searched for,
    /// exactly as the rollback test proves it exists. Panics when no
    /// position triggers.
    fn find_rollback_trigger(
        build: impl Fn() -> (RamBlocks, CardFs<RamBlocks>),
        staging: u32,
        path: &str,
        new_contents: &[u8],
        original: &[u8],
    ) -> u32 {
        for extra in 0.. {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(staging + extra));
            let result = fs.save_transactional(path, new_contents);
            if ram.corrupt_one_write_after.get().is_some() {
                break;
            }
            if matches!(
                result,
                Err(SaveError {
                    error: FsOpError::VerifyFailed,
                    ..
                })
            ) && fs.read_file(path).is_ok_and(|bytes| bytes == original)
            {
                return extra;
            }
        }
        panic!("no commit-verify-failing corruption position");
    }

    #[test]
    fn kept_backup_is_never_a_truncated_ghost() {
        // On FAT32 a delete truncates before removing the entry, so a
        // backup whose delete failed after a successful (verified) restore
        // may be a zero-byte ghost; reporting it as a kept backup would
        // point the user at a worthless file and bury the real error.
        // (FAT16, whose deletes never truncate, has its own test below.)
        let original = vec![0x55u8; 2048];
        let new_contents = vec![0xaau8; 2048];
        let staging = staging_write_count(&new_contents, &original);
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", &original).unwrap();
            (ram, fs)
        };
        let trigger = find_rollback_trigger(&build, staging, "/FILE.TXT", &new_contents, &original);
        // With the verify failure pinned, sweep a latched write failure
        // across the commit's remainder and the whole rollback: whenever a
        // backup is reported kept it must hold the original bytes, and a
        // completed restore must never report one.
        for extra in 0..120u32 {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(staging + trigger));
            ram.fail_writes_after
                .set(Some(staging + trigger + 1 + extra));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            ram.fail_writes.set(false);
            ram.fail_writes_after.set(None);
            ram.corrupt_one_write_after.set(None);
            let Err(save_error) = result else {
                continue;
            };
            if let Some(kept) = &save_error.backup_kept {
                // An unreadable backup (truncate freed its chain) is as
                // much a ghost as an empty one.
                assert_eq!(
                    fs.read_file(kept).unwrap_or_default(),
                    original,
                    "extra {extra}: kept backup does not hold the original"
                );
            }
            if fs
                .read_file("/FILE.TXT")
                .is_ok_and(|bytes| bytes == original)
                && matches!(save_error.error, FsOpError::VerifyFailed)
            {
                // The rollback restored the target from the backup. A
                // surviving backup that still holds every original byte
                // must be named -- the delete's DeviceError can strike
                // before any FAT write, leaving the full contents intact.
                // A ghost, partial, or absent backup has served its
                // purpose and must not be advertised.
                if fs
                    .read_file("/~WIO0000.BAK")
                    .is_ok_and(|bytes| bytes == original)
                {
                    assert_eq!(
                        save_error.backup_kept.as_deref(),
                        Some("/~WIO0000.BAK"),
                        "extra {extra}: intact full-content backup not named"
                    );
                } else {
                    assert_eq!(
                        save_error.backup_kept, None,
                        "extra {extra}: restored target advertises a ghost backup"
                    );
                }
            }
        }
    }

    #[test]
    fn fat32_refused_backup_delete_names_the_intact_backup() {
        // The FAT32 twin of the FAT16 kept-backup test below:
        // DeleteIncomplete from the backup's delete only proves
        // destruction was POSSIBLE. The truncate's opening reads (and a
        // truncate write the card refused) can fail with every original
        // byte intact, and the rollback must then name the backup instead
        // of silently stranding a full-size hidden file. A region outage
        // cannot reach this branch -- it would launder the delete's own
        // lookup into NotFound before the truncate ever runs -- so the
        // positioned single-shot knobs drive both fault phases.
        let original = vec![0x55u8; 2048];
        let new_contents = vec![0xaau8; 2048];
        let staging = staging_write_count(&new_contents, &original);
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", &original).unwrap();
            (ram, fs)
        };
        // Phase 1: find a corruption position that makes the commit's
        // verify fail and the rollback restore the target, exactly as
        // kept_backup_is_never_a_truncated_ghost does.
        let trigger = find_rollback_trigger(&build, staging, "/FILE.TXT", &new_contents, &original);
        // Phase 2: sweep a latched write death across the rollback. A
        // latch met at the truncate's first FAT write persists nothing,
        // so the backup keeps its full contents while its delete reports
        // DeleteIncomplete -- the restored target must name it.
        let mut proven_write = None;
        for extra in 0..120u32 {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(staging + trigger));
            ram.fail_writes_after
                .set(Some(staging + trigger + 1 + extra));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            ram.fail_writes.set(false);
            ram.fail_writes_after.set(None);
            ram.corrupt_one_write_after.set(None);
            let Err(save_error) = result else {
                continue;
            };
            if let Some(kept) = &save_error.backup_kept {
                assert_eq!(
                    fs.read_file(kept).unwrap_or_default(),
                    original,
                    "extra {extra}: kept backup does not hold the original"
                );
            }
            if fs
                .read_file("/FILE.TXT")
                .is_ok_and(|bytes| bytes == original)
                && matches!(save_error.error, FsOpError::VerifyFailed)
                && fs
                    .read_file("/~WIO0000.BAK")
                    .is_ok_and(|bytes| bytes == original)
            {
                // The restore landed and the backup provably still holds
                // every original byte: with reads healthy the readback
                // settles it, so it must be advertised.
                assert_eq!(
                    save_error.backup_kept.as_deref(),
                    Some("/~WIO0000.BAK"),
                    "extra {extra}: intact full-content backup not named"
                );
                if proven_write.is_none() && !fs.entry_exists("/~WIO0000.TMP").unwrap_or(true) {
                    // Only the backup's delete failed: the exact branch
                    // the readback fix distinguishes from a proven ghost.
                    proven_write = Some(extra);
                }
            }
        }
        let proven_write =
            proven_write.expect("no write latch position refused only the backup's delete");
        // Phase 3: land the fault on the backup delete's opening reads
        // instead -- a DeviceError with zero FAT writes, the reported
        // trigger. Twin-measure the proven run's reads, then sweep a
        // one-shot read fault over the tail where the backup delete
        // lives. A shot that instead hits the readback compare is the
        // accepted correlated-double-fault residue (suppressed), so only
        // the sweep as a whole must produce the named outcome.
        let total_reads = {
            let (ram, fs) = build();
            let before = ram.reads_seen.get();
            ram.corrupt_one_write_after.set(Some(staging + trigger));
            ram.fail_writes_after
                .set(Some(staging + trigger + 1 + proven_write));
            let _ = fs.save_transactional("/FILE.TXT", &new_contents);
            ram.fail_writes.set(false);
            ram.reads_seen.get() - before
        };
        let mut proven_read = false;
        for shot in total_reads.saturating_sub(40)..total_reads {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(staging + trigger));
            ram.fail_writes_after
                .set(Some(staging + trigger + 1 + proven_write));
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            ram.fail_writes.set(false);
            ram.fail_writes_after.set(None);
            ram.corrupt_one_write_after.set(None);
            ram.fail_one_read_after.set(None);
            let Err(save_error) = result else {
                continue;
            };
            if let Some(kept) = &save_error.backup_kept {
                assert_eq!(
                    fs.read_file(kept).unwrap_or_default(),
                    original,
                    "shot {shot}: kept backup does not hold the original"
                );
            }
            if fs
                .read_file("/FILE.TXT")
                .is_ok_and(|bytes| bytes == original)
                && matches!(save_error.error, FsOpError::VerifyFailed)
                && !fs.entry_exists("/~WIO0000.TMP").unwrap_or(true)
                && fs
                    .read_file("/~WIO0000.BAK")
                    .is_ok_and(|bytes| bytes == original)
                && save_error.backup_kept.as_deref() == Some("/~WIO0000.BAK")
            {
                proven_read = true;
            }
        }
        assert!(
            proven_read,
            "no read-fault position named an intact backup after a refused delete"
        );
    }

    /// The FAT16 twin of `staging_write_count`: writes consumed by a
    /// save's staging steps on the FAT16 fixture, measured on a twin with
    /// an identical write history.
    fn fat16_staging_write_count(new_contents: &[u8], original: &[u8]) -> u32 {
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/FILE.TXT", original).unwrap();
        let before = ram.writes_seen.get();
        fs.save_verified("/~WIO0000.TMP", new_contents).unwrap();
        fs.copy_file_contents(
            "/FILE.TXT",
            "/~WIO0000.BAK",
            Mode::ReadWriteCreate,
            &mut false,
        )
        .unwrap();
        ram.writes_seen.get() - before
    }

    #[test]
    fn fat16_refused_backup_delete_reports_the_kept_backup() {
        // FAT16 deletes never truncate (the FAT32-only reclaim is gated
        // off), so a backup whose delete was refused after a successful
        // restore still holds the complete original contents. Silently
        // dropping it would strand a full-size hidden file on the card;
        // unlike the FAT32 ghost case above, it must be reported.
        let original = vec![0x55u8; 2048];
        let new_contents = vec![0xaau8; 2048];
        let staging = fat16_staging_write_count(&new_contents, &original);
        let build = || {
            let (ram, layout) = fat16_media();
            let fs = CardFs::mount(ram.clone(), layout).unwrap();
            fs.save_verified("/FILE.TXT", &original).unwrap();
            (ram, fs)
        };
        // Find a corruption position that makes the commit's verify fail
        // and the rollback restore the target, exactly as the FAT32 test
        // above does.
        let trigger = find_rollback_trigger(&build, staging, "/FILE.TXT", &new_contents, &original);
        // With the verify failure pinned, sweep a latched write death
        // across the rollback. Wherever the restore landed but a
        // full-content backup survived, it must be named.
        let mut proven = false;
        for extra in 0..120u32 {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(staging + trigger));
            ram.fail_writes_after
                .set(Some(staging + trigger + 1 + extra));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            ram.fail_writes.set(false);
            ram.fail_writes_after.set(None);
            ram.corrupt_one_write_after.set(None);
            let Err(save_error) = result else {
                continue;
            };
            if let Some(kept) = &save_error.backup_kept {
                // On FAT16 a kept backup is never a ghost: no truncate
                // ever ran, so it must hold the complete original.
                assert_eq!(
                    fs.read_file(kept).unwrap_or_default(),
                    original,
                    "extra {extra}: kept backup does not hold the original"
                );
            }
            if fs
                .read_file("/FILE.TXT")
                .is_ok_and(|bytes| bytes == original)
                && fs.entry_exists("/~WIO0000.BAK").unwrap_or(false)
                && matches!(save_error.error, FsOpError::VerifyFailed)
            {
                // The restore landed and a full-content backup survives on
                // the card: it must be advertised, never silently dropped.
                assert_eq!(
                    save_error.backup_kept.as_deref(),
                    Some("/~WIO0000.BAK"),
                    "extra {extra}: full-content backup survived unreported"
                );
                if !fs.entry_exists("/~WIO0000.TMP").unwrap_or(true) {
                    // The temporary's delete succeeded and only the
                    // backup's delete was refused: the exact branch the
                    // fix distinguishes from a completed cleanup.
                    proven = true;
                }
            }
        }
        assert!(proven, "the sweep never refused only the backup's delete");
    }

    #[test]
    fn save_deletes_its_own_temporary_when_a_missed_backup_blocks_staging() {
        // A FAT-region outage during the staging-name scan launders both
        // confirmed lookups into "absent" (the correlated double-flake
        // residue single flakes cannot produce), so the save picks a
        // suffix whose ~WIO*.BAK actually exists. The temporary then
        // stages and verifies, and the backup copy's creating open --
        // against a healed card -- finds the real backup and refuses with
        // AlreadyExists. No rollback follows (the blocking entry
        // pre-existed and must survive), but the temporary is this save's
        // own verified creation and must not be stranded.
        let original = b"original contents".to_vec();
        let mut proven = false;
        for budget in 1..=8u32 {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            // Fill the directory's first cluster (16 entries: dot, dotdot,
            // and 14 files) so the staging names' lookups must step the
            // FAT chain past it -- the step a region outage launders.
            for index in 0..14u32 {
                fs.create_empty(&alloc::format!("/DIR/F{index:02}.TXT"))
                    .unwrap();
            }
            // Lands in the directory's second cluster.
            fs.save_verified("/DIR/~WIO0000.BAK", b"pre-existing backup")
                .unwrap();
            // Free a first-cluster slot for the target so its pre-check
            // lookup early-breaks without stepping the FAT.
            fs.delete_verified("/DIR/F00.TXT", false).unwrap();
            fs.save_verified("/DIR/FILE.TXT", &original).unwrap();
            fs.drop_block_cache();
            let geometry = fat32_geometry(131_072, 0).unwrap();
            let fat_start = geometry.partition_start + 32;
            ram.fail_reads_in
                .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
            ram.fail_reads_in_budget.set(Some(budget));
            let result = fs.save_transactional("/DIR/FILE.TXT", b"new contents");
            ram.fail_reads_in.set(None);
            ram.fail_reads_in_budget.set(None);
            let Err(SaveError {
                error: FsOpError::AlreadyExists,
                backup_kept,
            }) = result
            else {
                // Smaller budgets self-correct (a surviving re-ask finds
                // the backup and the save proceeds on a fresh suffix);
                // larger ones fail the temporary's staging with a device
                // error. Neither is this test's branch.
                continue;
            };
            proven = true;
            assert_eq!(backup_kept, None, "budget {budget}");
            // The save's own verified temporary must have been removed,
            // not stranded until a PC cleanup.
            let (page, _) = fs.read_directory_page("/DIR", 0, 32).unwrap();
            let stranded: Vec<String> = page
                .into_iter()
                .map(|item| item.name)
                .filter(|name| name.starts_with("~WIO"))
                .collect();
            assert_eq!(
                stranded,
                ["~WIO0000.BAK"],
                "budget {budget}: staging debris was left behind"
            );
            // The pre-existing backup -- what the rollback refusal exists
            // to protect -- and the target must both be untouched.
            assert_eq!(
                fs.read_file("/DIR/~WIO0000.BAK").unwrap(),
                b"pre-existing backup",
                "budget {budget}"
            );
            assert_eq!(
                fs.read_file("/DIR/FILE.TXT").unwrap(),
                original,
                "budget {budget}"
            );
        }
        assert!(
            proven,
            "no budget produced the laundered-scan AlreadyExists"
        );
    }

    #[test]
    fn hidden_destination_collision_never_deletes_the_real_tree() {
        // A FAT-region outage can launder a folder move's destination
        // pre-check AND make_dir_in_dir's own existence scan (both walks
        // discard chain-step read errors), after which the healed card
        // appends a true duplicate DEST entry: every later by-name
        // resolution reaches the user's REAL pre-existing tree first. The
        // child copy then collides with the real tree's child
        // (AlreadyExists after the root landed) -- or, with the outage
        // alive one read longer, the free-slot walk masks the fault as
        // NotEnoughSpace before anything is created. In both cases a
        // by-name cleanup could destroy the real tree, so the move must
        // refuse it and report the debris as CleanupIncomplete. On an
        // honest card a collision under a root this operation created is
        // unreachable (unique source names, fresh empty root, and a
        // visible destination stops the pre-check), so the laundered
        // route is the reachable path.
        let mut proven_collision = false;
        let mut proven_masked = false;
        for budget in 1..=10u32 {
            let (ram, fs) = formatted_fs(131_072);
            // /SRC lands in the root's first cluster; the pads fill it
            // (16 entries per 512-byte cluster, and the root has no dot
            // entries) so /DEST's entry -- and any duplicate of it --
            // sits past the FAT chain step a region outage launders.
            fs.create_directory_verified("/SRC").unwrap();
            for index in 0..15u32 {
                fs.create_empty(&alloc::format!("/P{index:02}.TXT"))
                    .unwrap();
            }
            fs.create_directory_verified("/DEST").unwrap();
            fs.save_verified("/DEST/A.TXT", b"precious destination")
                .unwrap();
            fs.save_verified("/SRC/A.TXT", b"source payload").unwrap();
            fs.drop_block_cache();
            let geometry = fat32_geometry(131_072, 0).unwrap();
            let fat_start = geometry.partition_start + 32;
            ram.fail_reads_in
                .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
            ram.fail_reads_in_budget.set(Some(budget));
            let result = fs.move_entry_verified("/SRC", "/DEST");
            ram.fail_reads_in.set(None);
            ram.fail_reads_in_budget.set(None);
            let dest_entries = || {
                let (page, _) = fs.read_directory_page("/", 0, 32).unwrap();
                page.into_iter().filter(|item| item.name == "DEST").count()
            };
            match result {
                Err(FsOpError::AlreadyExists) => {
                    // The outage healed early enough for some walk to see
                    // the real /DEST: refused with nothing created.
                    assert_eq!(dest_entries(), 1, "budget {budget}");
                }
                Err(FsOpError::CleanupIncomplete(inner)) => {
                    if matches!(*inner, FsOpError::AlreadyExists) {
                        // The duplicate root landed and the child copy
                        // collided with the real tree's child: the
                        // ambiguity guard must refuse the by-name cleanup
                        // and report the stranded duplicate.
                        assert_eq!(dest_entries(), 2, "budget {budget}");
                        // A retry against the healed card stops at the
                        // real destination, exactly what the report
                        // promises.
                        assert!(
                            matches!(
                                fs.move_entry_verified("/SRC", "/DEST"),
                                Err(FsOpError::AlreadyExists)
                            ),
                            "budget {budget}"
                        );
                        proven_collision = true;
                    } else {
                        // The outage survived into the free-slot walk:
                        // the masked fault aborted the root creation and
                        // the empty-only guard refused to touch the
                        // non-empty real tree.
                        assert_eq!(dest_entries(), 1, "budget {budget}");
                        proven_masked = true;
                    }
                }
                // Smaller budgets self-correct, larger ones surface
                // device errors from later steps; neither is this test's
                // branch.
                _ => {}
            }
            // Whatever the outcome, the real destination tree and the
            // source must be untouched: no fault pattern may convert a
            // refused move into destruction of either.
            assert_eq!(
                fs.read_file("/DEST/A.TXT").unwrap(),
                b"precious destination",
                "budget {budget}: the real destination tree was mutated"
            );
            assert_eq!(
                fs.read_file("/SRC/A.TXT").unwrap(),
                b"source payload",
                "budget {budget}: the source was mutated"
            );
        }
        assert!(
            proven_collision,
            "no budget produced the duplicate-root collision"
        );
        assert!(
            proven_masked,
            "no budget produced the masked free-slot fault"
        );
    }

    #[test]
    fn empty_destination_dir_survives_masked_make_dir_failure() {
        // The empty twin of hidden_destination_collision...: with the real
        // /DEST empty, the old empty-means-ours cleanup deleted the user's
        // directory after a masked root-creation failure. Cleanup now
        // removes only provable cluster-0 debris and refuses everything
        // else, so no fault pattern may cost the user their directory.
        let mut proven_refusal = false;
        for budget in 1..=10u32 {
            let (ram, fs) = formatted_fs(131_072);
            // Same layout as the template: /SRC plus 15 pads fill the
            // root's first cluster, so /DEST's entry sits past the FAT
            // chain step a region outage launders.
            fs.create_directory_verified("/SRC").unwrap();
            for index in 0..15u32 {
                fs.create_empty(&alloc::format!("/P{index:02}.TXT"))
                    .unwrap();
            }
            fs.create_directory_verified("/DEST").unwrap();
            fs.save_verified("/SRC/A.TXT", b"source payload").unwrap();
            fs.drop_block_cache();
            let geometry = fat32_geometry(131_072, 0).unwrap();
            let fat_start = geometry.partition_start + 32;
            ram.fail_reads_in
                .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
            ram.fail_reads_in_budget.set(Some(budget));
            let result = fs.move_entry_verified("/SRC", "/DEST");
            ram.fail_reads_in.set(None);
            ram.fail_reads_in_budget.set(None);
            // The user's directory must survive every fault pattern: a
            // refusal may strand a duplicate, but the pre-existing
            // (first-listed) /DEST must stay a directory.
            let (page, _) = fs.read_directory_page("/", 0, 32).unwrap();
            let dest_count = page
                .iter()
                .filter(|item| item.name == "DEST" && item.is_dir)
                .count();
            assert!(
                dest_count >= 1,
                "budget {budget}: the empty pre-existing directory was deleted"
            );
            if let Err(FsOpError::CleanupIncomplete(inner)) = &result {
                if !matches!(**inner, FsOpError::AlreadyExists) {
                    // The masked free-slot fault aborted the root creation
                    // with nothing created, and the cleanup refused to act
                    // on the real (non-debris) directory the name resolves
                    // to -- the branch the old empty-only rule turned into
                    // deletion of the user's directory.
                    proven_refusal = true;
                }
            }
            if result.is_err() {
                assert_eq!(
                    fs.read_file("/SRC/A.TXT").unwrap(),
                    b"source payload",
                    "budget {budget}: a failed move mutated the source"
                );
            }
        }
        assert!(proven_refusal, "no budget exercised the refusal arm");
    }

    /// Mark every free FAT16 entry as used (0xFFF7, "bad cluster") directly
    /// in both FAT copies. Filling by writing files cannot reliably take
    /// the last clusters -- the manager's free-cluster search and boundary
    /// pre-allocation get in the way -- and production code only ever
    /// observes the resulting full FAT. Returns the patched positions so a
    /// test can free them again.
    fn exhaust_fat16_free_clusters(ram: &RamBlocks) -> Vec<(usize, usize)> {
        let mut patched = Vec::new();
        let fat_start = (FAT16_START + FAT16_RESERVED) as usize;
        let mut blocks = ram.blocks.borrow_mut();
        for sector in 0..16usize {
            for offset in (0..512).step_by(2) {
                let cluster = (sector * 512 + offset) / 2;
                if cluster < 2 || cluster >= FAT16_CLUSTERS as usize + 2 {
                    continue;
                }
                if blocks[fat_start + sector].contents[offset..offset + 2] == [0, 0] {
                    blocks[fat_start + sector].contents[offset..offset + 2]
                        .copy_from_slice(&[0xf7, 0xff]);
                    blocks[fat_start + 16 + sector].contents[offset..offset + 2]
                        .copy_from_slice(&[0xf7, 0xff]);
                    patched.push((sector, offset));
                }
            }
        }
        patched
    }

    /// Undo [`exhaust_fat16_free_clusters`].
    fn restore_fat16_clusters(ram: &RamBlocks, patched: &[(usize, usize)]) {
        let fat_start = (FAT16_START + FAT16_RESERVED) as usize;
        let mut blocks = ram.blocks.borrow_mut();
        for &(sector, offset) in patched {
            blocks[fat_start + sector].contents[offset..offset + 2].copy_from_slice(&[0, 0]);
            blocks[fat_start + 16 + sector].contents[offset..offset + 2].copy_from_slice(&[0, 0]);
        }
    }

    #[test]
    fn full_card_move_debris_is_surgically_removed_and_retry_unblocked() {
        // FAT16 for genuine exhaustion (see
        // genuine_disk_full_rolls_back_and_reports_full). make_dir persists
        // the destination entry with cluster 0 before its cluster
        // allocation fails on the full card; embedded-sdmmc remaps that
        // entry to the volume root, so the manager can neither list nor
        // delete it -- only the surgical cleanup can. The move must report
        // the honest full-disk error with the destination name clean, and
        // a retry after freeing space must succeed.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.create_directory_verified("/SRC").unwrap();
        fs.save_verified("/SRC/A.TXT", b"payload").unwrap();
        let patched = exhaust_fat16_free_clusters(&ram);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        assert_eq!(fs.free_space_bytes(), Some(0));
        let error = fs
            .move_entry_verified("/SRC", "/DEST")
            .expect_err("the full card cannot hold the destination root");
        assert!(
            matches!(
                error,
                FsOpError::Fs(
                    embedded_sdmmc::Error::DiskFull | embedded_sdmmc::Error::NotEnoughSpace
                )
            ),
            "expected a clean full-disk report (debris removed), got {error:?}"
        );
        assert!(
            !fs.entry_exists("/DEST").unwrap(),
            "the cluster-0 debris entry survived the cleanup"
        );
        assert_eq!(fs.read_file("/SRC/A.TXT").unwrap(), b"payload");
        // With space freed and the debris gone, the retry must succeed.
        restore_fat16_clusters(&ram, &patched);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        fs.move_entry_verified("/SRC", "/DEST").unwrap();
        assert_eq!(fs.read_file("/DEST/A.TXT").unwrap(), b"payload");
    }

    #[test]
    fn delete_verified_on_root_alias_debris_never_touches_root_children() {
        // Interrupted-make_dir debris aliases the volume root: a recursive
        // delete walking "its" children would walk -- and destroy -- the
        // real root's. The debris arm must remove the bare entry
        // surgically and leave every real entry intact.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/PRECIOUS.TXT", b"precious root child")
            .unwrap();
        let _patched = exhaust_fat16_free_clusters(&ram);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        assert_eq!(fs.free_space_bytes(), Some(0));
        let error = fs
            .create_directory_verified("/DEBRIS")
            .expect_err("the full card cannot allocate the directory's cluster");
        assert!(matches!(
            error,
            FsOpError::Fs(embedded_sdmmc::Error::DiskFull | embedded_sdmmc::Error::NotEnoughSpace)
        ));
        // The failed creation strands the cluster-0 entry (create has no
        // rollback of its own).
        assert!(fs.entry_exists("/DEBRIS").unwrap());
        fs.delete_verified("/DEBRIS", true).unwrap();
        assert!(
            !fs.entry_exists("/DEBRIS").unwrap(),
            "the debris entry survived"
        );
        assert_eq!(
            fs.read_file("/PRECIOUS.TXT").unwrap(),
            b"precious root child"
        );
    }

    #[test]
    fn debris_path_components_are_refused_before_aliasing_root() {
        // Opening a cluster-0 debris entry as a directory aliases the
        // volume root, so a path that merely traverses THROUGH the debris
        // would list -- and mutate -- the real root behind an innocent-
        // looking folder. Every path-based operation must refuse at the
        // component; only the debris leaf itself stays deletable.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/PRECIOUS.TXT", b"precious root child")
            .unwrap();
        let _patched = exhaust_fat16_free_clusters(&ram);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        fs.create_directory_verified("/DEBRIS")
            .expect_err("the full card cannot allocate the directory's cluster");
        assert!(fs.entry_exists("/DEBRIS").unwrap());
        // Listing "its" children would render the real root's.
        assert!(matches!(
            fs.read_directory_page("/DEBRIS", 0, 8),
            Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster))
        ));
        // A delete through the alias would destroy the real root child.
        assert!(matches!(
            fs.delete_verified("/DEBRIS/PRECIOUS.TXT", false),
            Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster))
        ));
        // A save into the debris would create the file in the real root.
        assert!(matches!(
            fs.save_transactional("/DEBRIS/NEW.TXT", b"landed in root"),
            Err(SaveError {
                error: FsOpError::Fs(embedded_sdmmc::Error::BadCluster),
                ..
            })
        ));
        // A move destination inside the debris would land in the real
        // root; the pre-check's find_entry(to) must refuse first.
        assert!(matches!(
            fs.move_entry_verified("/PRECIOUS.TXT", "/DEBRIS/MOVED.TXT"),
            Err(FsOpError::Fs(embedded_sdmmc::Error::BadCluster))
        ));
        assert_eq!(
            fs.read_file("/PRECIOUS.TXT").unwrap(),
            b"precious root child"
        );
        assert!(!fs.entry_exists("/NEW.TXT").unwrap());
        assert!(!fs.entry_exists("/MOVED.TXT").unwrap());
        // The debris itself is a leaf of its (real) parent, not a
        // traversal, and must stay deletable.
        fs.delete_verified("/DEBRIS", true).unwrap();
        assert!(!fs.entry_exists("/DEBRIS").unwrap());
    }

    #[test]
    fn debris_erase_clears_its_same_sector_lfn_chain() {
        // Foreign-created debris can carry LFN entries ahead of the
        // short-name slot; the surgical erase must mark checksum-matched
        // ones unused too, or the orphaned chain attaches to the next
        // entry created in the freed slot. A preceding LFN slot with a
        // foreign checksum belongs to some other chain and must survive.
        fn lfn_checksum(name: &[u8; 11]) -> u8 {
            name.iter().fold(0u8, |sum, &byte| {
                ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte)
            })
        }
        fn plant_lfn(contents: &mut [u8], offset: usize, csum: u8) {
            let slot = &mut contents[offset..offset + 32];
            slot.fill(0xFF);
            slot[0] = 0x41; // sequence 1 with the last-entry flag
            slot[11] = 0x0F; // the LFN attribute
            slot[12] = 0x00;
            slot[13] = csum;
            slot[26] = 0x00; // an LFN slot's cluster field must be zero
            slot[27] = 0x00;
        }
        fn plant_debris(contents: &mut [u8], offset: usize, name: &[u8; 11]) {
            let slot = &mut contents[offset..offset + 32];
            slot.fill(0x00);
            slot[0..11].copy_from_slice(name);
            slot[11] = 0x10; // directory attribute; first cluster stays 0
        }
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/KEEP.TXT", b"unrelated").unwrap();
        let debris_name = *b"DEBRIS     ";
        let twin_name = *b"DEBRIS2    ";
        // Four consecutive free slots in the FAT16 root region hold:
        // [matching LFN][debris SFN][foreign LFN][second debris SFN].
        let root_start = (FAT16_START + FAT16_RESERVED + 32) as usize;
        let mut planted = None;
        'search: for sector in root_start..root_start + FAT16_ROOT_SECTORS as usize {
            let blocks = ram.blocks.borrow();
            for slot in 0..=12usize {
                let offset = slot * 32;
                if (0..4).all(|entry| blocks[sector].contents[offset + entry * 32] == 0x00) {
                    planted = Some((sector, offset));
                    break 'search;
                }
            }
        }
        let (sector, base) = planted.expect("no free root-directory slots");
        {
            let mut blocks = ram.blocks.borrow_mut();
            let contents = &mut blocks[sector].contents;
            plant_lfn(contents, base, lfn_checksum(&debris_name));
            plant_debris(contents, base + 32, &debris_name);
            plant_lfn(
                contents,
                base + 64,
                lfn_checksum(&twin_name).wrapping_add(1),
            );
            plant_debris(contents, base + 96, &twin_name);
        }
        fs.drop_block_cache();
        fs.delete_verified("/DEBRIS", true).unwrap();
        fs.delete_verified("/DEBRIS2", true).unwrap();
        let blocks = ram.blocks.borrow();
        assert_eq!(
            blocks[sector].contents[base], 0xE5,
            "the matching LFN slot was not erased"
        );
        assert_eq!(
            blocks[sector].contents[base + 32],
            0xE5,
            "the debris slot was not erased"
        );
        assert_eq!(
            blocks[sector].contents[base + 64],
            0x41,
            "a foreign-checksum LFN slot was erased"
        );
        assert_eq!(
            blocks[sector].contents[base + 96],
            0xE5,
            "the second debris slot was not erased"
        );
    }

    #[test]
    fn dir_move_to_missing_parent_reports_the_true_not_found() {
        // A directory move whose destination parent does not exist
        // provably creates nothing: the cleanup resolves Absent (the
        // parent's absence is confirmed by with_dir's double-checked
        // component lookups) and the true NotFound passes through --
        // never CleanupIncomplete ("partial copy left") for an operation
        // that left no debris.
        let (_ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/SRC").unwrap();
        fs.save_verified("/SRC/A.TXT", b"payload").unwrap();
        let error = fs
            .move_entry_verified("/SRC", "/GONE/DEST")
            .expect_err("the destination parent does not exist");
        assert!(
            error.is_not_found(),
            "expected the true NotFound, got {error:?}"
        );
        assert_eq!(fs.read_file("/SRC/A.TXT").unwrap(), b"payload");
        // The file arm agrees (copy_file_verified's created=false path).
        let error = fs
            .move_entry_verified("/SRC/A.TXT", "/GONE/B.TXT")
            .expect_err("the destination parent does not exist");
        assert!(error.is_not_found());
        assert_eq!(fs.read_file("/SRC/A.TXT").unwrap(), b"payload");
    }

    #[test]
    fn move_refuses_debris_containing_source_before_touching_destination() {
        // check_subtree_mutable must refuse interrupted-make_dir debris for
        // a move: copy_dir_children hard-fails on it, so waving it through
        // would create the destination root and copy children for an
        // operation guaranteed to abort midway. Plant the debris inside the
        // source by exhausting the card during a nested create.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.create_directory_verified("/SRC").unwrap();
        fs.save_verified("/SRC/A.TXT", b"payload").unwrap();
        let patched = exhaust_fat16_free_clusters(&ram);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        fs.create_directory_verified("/SRC/DEBRIS")
            .expect_err("the full card cannot allocate the directory's cluster");
        assert!(fs.entry_exists("/SRC/DEBRIS").unwrap());
        // Space is back, but the debris child remains: the pre-flight must
        // refuse before anything lands at the destination.
        restore_fat16_clusters(&ram, &patched);
        fs.drop_block_cache();
        fs.mark_free_space_stale();
        let error = fs
            .move_entry_verified("/SRC", "/DEST")
            .expect_err("a debris-containing source cannot be moved");
        assert!(
            matches!(error, FsOpError::Fs(embedded_sdmmc::Error::BadCluster)),
            "expected the debris refusal, got {error:?}"
        );
        assert!(
            !fs.entry_exists("/DEST").unwrap(),
            "the refused move mutated the destination"
        );
        assert_eq!(fs.read_file("/SRC/A.TXT").unwrap(), b"payload");
        // Deleting the debris repairs the source; the retry then succeeds.
        fs.delete_verified("/SRC/DEBRIS", true).unwrap();
        fs.move_entry_verified("/SRC", "/DEST").unwrap();
        assert_eq!(fs.read_file("/DEST/A.TXT").unwrap(), b"payload");
    }

    #[test]
    fn same_named_volume_label_blocks_by_name_rollback_delete() {
        // The by-name delete a Unique verdict authorizes matches raw
        // 11-byte names with no attribute filter, so a volume label
        // sharing a root-level file's name is a real candidate for the
        // erase. The resolver must count it toward ambiguity and refuse,
        // stranding debris rather than gambling on which entry the
        // delete would hit.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.create_empty("/BACKUP").unwrap();
        fs.drop_block_cache();
        let root_dir_lba = FAT16_START + FAT16_RESERVED + 32;
        let slot = {
            let mut blocks = ram.blocks.borrow_mut();
            let sector = &mut blocks[root_dir_lba as usize].contents;
            let slot = (0..512)
                .step_by(32)
                .find(|&offset| sector[offset] == 0x00)
                .expect("no free root directory slot");
            let mut name = [0x20u8; 11];
            name[..6].copy_from_slice(b"BACKUP");
            sector[slot..slot + 11].copy_from_slice(&name);
            sector[slot + 11] = 0x08; // volume-label attribute
            slot
        };
        fs.drop_block_cache();
        assert!(
            matches!(
                fs.resolve_destination_name("/BACKUP"),
                NameResolution::Unsure
            ),
            "a same-named volume label must make the name ambiguous"
        );
        assert!(
            fs.guarded_rollback_delete("/BACKUP", false) == CleanupOutcome::Blocked,
            "the guarded delete acted despite the label ambiguity"
        );
        // Both survivors: the file is still listed...
        assert!(fs.entry_exists("/BACKUP").unwrap());
        // ...and the label's slot is untouched (not 0xE5-erased).
        let sector = read_device_sector(&ram, root_dir_lba).unwrap();
        assert_eq!(sector[slot], b'B', "the volume label was erased");
        assert_eq!(sector[slot + 11], 0x08);
    }

    #[test]
    fn hidden_file_destination_collision_never_truncates_the_real_file() {
        // The file-arm mirror of
        // hidden_destination_collision_never_deletes_the_real_tree: a
        // laundered pre-check plus the creating open's laundered scan
        // append a duplicate /DEST.TXT entry, the copy's verify then
        // resolves the name to the user's first-listed real file and
        // fails, and the rollback's by-name delete -- unguarded before
        // this fix -- would truncate that real file. The guarded delete
        // must refuse on ambiguity instead.
        let mut proven_duplicate = false;
        for budget in 1..=10u32 {
            let (ram, fs) = formatted_fs(131_072);
            // /SRC.TXT plus 15 pads fill the root's first cluster (16
            // entries per 512-byte cluster, and the root has no dot
            // entries); /DEST.TXT lands past the chain step a region
            // outage launders.
            fs.save_verified("/SRC.TXT", b"source payload").unwrap();
            for index in 0..15u32 {
                fs.create_empty(&alloc::format!("/P{index:02}.TXT"))
                    .unwrap();
            }
            fs.save_verified("/DEST.TXT", b"precious destination")
                .unwrap();
            fs.drop_block_cache();
            let geometry = fat32_geometry(131_072, 0).unwrap();
            let fat_start = geometry.partition_start + 32;
            ram.fail_reads_in
                .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
            ram.fail_reads_in_budget.set(Some(budget));
            let result = fs.move_entry_verified("/SRC.TXT", "/DEST.TXT");
            ram.fail_reads_in.set(None);
            ram.fail_reads_in_budget.set(None);
            // Whatever the outcome: the real destination's contents are
            // sacred (by-name reads resolve to the first-listed entry,
            // the user's file), and a failed move must leave the source
            // intact.
            assert_eq!(
                fs.read_file("/DEST.TXT").unwrap(),
                b"precious destination",
                "budget {budget}: the real destination was mutated"
            );
            if result.is_err() {
                assert_eq!(
                    fs.read_file("/SRC.TXT").unwrap(),
                    b"source payload",
                    "budget {budget}: a failed move mutated the source"
                );
            }
            let dest_entries = {
                let (page, _) = fs.read_directory_page("/", 0, 32).unwrap();
                page.into_iter()
                    .filter(|item| item.name == "DEST.TXT")
                    .count()
            };
            if matches!(&result, Err(FsOpError::CleanupIncomplete(inner))
                if matches!(**inner, FsOpError::VerifyFailed))
                && dest_entries == 2
            {
                // The duplicate landed, the verify resolved to the real
                // file, and the guard refused the by-name delete. A retry
                // against the healed card stops at the real destination,
                // exactly what the report promises.
                assert!(
                    matches!(
                        fs.move_entry_verified("/SRC.TXT", "/DEST.TXT"),
                        Err(FsOpError::AlreadyExists)
                    ),
                    "budget {budget}"
                );
                proven_duplicate = true;
            }
        }
        assert!(
            proven_duplicate,
            "no budget produced the duplicate-entry refusal"
        );
    }

    #[test]
    fn cleanup_guard_flake_refuses_instead_of_deleting() {
        // The guard's walks run on an already-faulty card. A single
        // transient read flake must now HEAL -- the resolver re-asks
        // (RESOLVE_RETRY_ATTEMPTS), absorbing it the way the old retried
        // delete did -- while a persistent outage inside the guard must
        // still refuse (CleanupIncomplete, destination still listed),
        // never delete on a half-seen listing. The source must survive
        // every shot of both sweeps.
        // Find a corruption position that fails the copy's verify with
        // the ordinary cleaned outcome.
        let trigger = 'found: {
            for extra in 0..200u32 {
                let (ram, fs) = formatted_fs(131_072);
                fs.save_verified("/A.TXT", b"source payload").unwrap();
                ram.corrupt_one_write_after.set(Some(extra));
                let result = fs.move_entry_verified("/A.TXT", "/B.TXT");
                let spent = ram.corrupt_one_write_after.get().is_none();
                ram.corrupt_one_write_after.set(None);
                if spent
                    && matches!(result, Err(FsOpError::VerifyFailed))
                    && !fs.entry_exists("/B.TXT").unwrap()
                {
                    break 'found extra;
                }
            }
            panic!("no corruption position fails the copy verify");
        };
        // Twin-measure the reads the failing move consumes, then sweep a
        // one-shot read fault across that window.
        let total_reads = {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/A.TXT", b"source payload").unwrap();
            let before = ram.reads_seen.get();
            ram.corrupt_one_write_after.set(Some(trigger));
            let _ = fs.move_entry_verified("/A.TXT", "/B.TXT");
            ram.corrupt_one_write_after.set(None);
            ram.reads_seen.get() - before
        };
        // Phase 1: single-shot flakes. Every one must heal -- refusing on
        // a lone flake is the stranding regression the resolver's retries
        // exist to prevent.
        let mut proven_cleaned = false;
        for shot in 0..total_reads {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/A.TXT", b"source payload").unwrap();
            ram.corrupt_one_write_after.set(Some(trigger));
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.move_entry_verified("/A.TXT", "/B.TXT");
            ram.corrupt_one_write_after.set(None);
            ram.fail_one_read_after.set(None);
            assert_eq!(
                fs.read_file("/A.TXT").unwrap(),
                b"source payload",
                "shot {shot}: the source was mutated"
            );
            match result {
                Err(FsOpError::CleanupIncomplete(_)) => {
                    panic!("shot {shot}: a single transient flake refused the cleanup");
                }
                Err(FsOpError::VerifyFailed) => {
                    if !fs.entry_exists("/B.TXT").unwrap() {
                        proven_cleaned = true;
                    }
                }
                // A shot landing before the corruption changes the failure
                // shape entirely, and one landing after the cleanup has no
                // effect; both are out of this test's scope as long as the
                // source assertion above holds.
                _ => {}
            }
        }
        assert!(proven_cleaned, "no shot kept the ordinary cleaned outcome");
        // Phase 2: the same sweep with each flake stretched into an
        // outage that outlasts every retry layer. Somewhere in the window
        // the guard must refuse, and a refusal must leave the partial
        // destination listed: that is the debris the report promises.
        let mut proven_refused = false;
        for shot in 0..total_reads {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/A.TXT", b"source payload").unwrap();
            ram.corrupt_one_write_after.set(Some(trigger));
            ram.fail_one_read_burst.set(64);
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.move_entry_verified("/A.TXT", "/B.TXT");
            ram.corrupt_one_write_after.set(None);
            ram.fail_one_read_after.set(None);
            ram.fail_one_read_burst.set(0);
            ram.fail_next_reads.set(0);
            assert_eq!(
                fs.read_file("/A.TXT").unwrap(),
                b"source payload",
                "burst shot {shot}: the source was mutated"
            );
            if let Err(FsOpError::CleanupIncomplete(_)) = result {
                assert!(
                    fs.entry_exists("/B.TXT").unwrap(),
                    "burst shot {shot}: refused cleanup but nothing listed"
                );
                proven_refused = true;
            }
        }
        assert!(proven_refused, "no outage refused the guarded cleanup");
    }

    #[test]
    fn intact_probe_never_vouches_for_zero_size_or_oversized_files() {
        // The probe reads the entry's size *after* a failed truncate, so a
        // size of 0 is exactly what a size-zeroing entry write that
        // persisted despite reporting an error leaves behind: it must
        // never pass as proof the contents survived (an originally empty
        // file loses nothing to a truncate, so its conservative verdict is
        // harmless). Oversized files keep the conservative verdict instead
        // of stalling the single-threaded UI on a full readback.
        let (_ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/DIR").unwrap();
        fs.create_empty("/DIR/EMPTY.TXT").unwrap();
        fs.save_verified("/DIR/SMALL.TXT", &[0x22u8; 2048]).unwrap();
        fs.save_verified("/DIR/BIG.BIN", &vec![0x33u8; INTACT_PROBE_MAX_BYTES + 1])
            .unwrap();
        let dir = fs.open_dir_path("/DIR").unwrap();
        let small = fs.file_reads_back_intact(dir, "SMALL.TXT");
        let empty = fs.file_reads_back_intact(dir, "EMPTY.TXT");
        let big = fs.file_reads_back_intact(dir, "BIG.BIN");
        let _ = fs.mgr.close_dir(dir);
        assert!(small, "a healthy in-bounds file must prove itself intact");
        assert!(
            !empty,
            "a zero-size entry passed as proof of intact contents"
        );
        assert!(
            !big,
            "an oversized file was probed instead of kept conservative"
        );
    }

    #[test]
    fn new_file_save_rollback_flake_reports_stranded_target() {
        // The save twin of cleanup_guard_flake_refuses_instead_of_deleting:
        // a new-file save whose commit verify fails rolls back through the
        // guarded delete. A single read flake inside the guard's walks now
        // heals (the resolver re-asks), so the rollback still removes the
        // partial target; a persistent outage strands it at the user's own
        // path -- debris the next save would even stage as the "original"
        // -- and that refusal must surface as CleanupIncomplete, never as
        // the bare original error. Take the *last* qualifying corruption
        // position so the trigger lands in the commit, not the staging
        // write.
        let mut found = None;
        for extra in 0..300u32 {
            let (ram, fs) = formatted_fs(131_072);
            ram.corrupt_one_write_after.set(Some(extra));
            let result = fs.save_transactional("/NEW.TXT", b"fresh contents");
            let spent = ram.corrupt_one_write_after.get().is_none();
            ram.corrupt_one_write_after.set(None);
            if spent
                && matches!(
                    &result,
                    Err(SaveError {
                        error: FsOpError::VerifyFailed,
                        ..
                    })
                )
                && !fs.entry_exists("/NEW.TXT").unwrap()
            {
                found = Some(extra);
            }
        }
        let trigger = found.expect("no corruption position fails the commit verify");
        // Twin-measure the reads the failing save consumes, then sweep a
        // one-shot read fault across that window.
        let total_reads = {
            let (ram, fs) = formatted_fs(131_072);
            let before = ram.reads_seen.get();
            ram.corrupt_one_write_after.set(Some(trigger));
            let _ = fs.save_transactional("/NEW.TXT", b"fresh contents");
            ram.corrupt_one_write_after.set(None);
            ram.reads_seen.get() - before
        };
        // Phase 1: single-shot flakes must heal -- never a refused
        // rollback, and never a stranded target passed off as the bare
        // VerifyFailed.
        for shot in 0..total_reads {
            let (ram, fs) = formatted_fs(131_072);
            ram.corrupt_one_write_after.set(Some(trigger));
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.save_transactional("/NEW.TXT", b"fresh contents");
            ram.corrupt_one_write_after.set(None);
            ram.fail_one_read_after.set(None);
            let Err(save_error) = result else {
                continue;
            };
            let stranded = fs.entry_exists("/NEW.TXT").unwrap();
            assert!(
                !(stranded && matches!(save_error.error, FsOpError::VerifyFailed)),
                "shot {shot}: partial target stranded but reported as bare VerifyFailed"
            );
            assert!(
                !matches!(save_error.error, FsOpError::CleanupIncomplete(_)),
                "shot {shot}: a single transient flake refused the rollback"
            );
        }
        // Phase 2: stretch each flake into an outage that outlasts every
        // retry layer; somewhere in the window the guard must refuse and
        // report the stranded target.
        let mut proven_reported = false;
        for shot in 0..total_reads {
            let (ram, fs) = formatted_fs(131_072);
            ram.corrupt_one_write_after.set(Some(trigger));
            ram.fail_one_read_burst.set(64);
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.save_transactional("/NEW.TXT", b"fresh contents");
            ram.corrupt_one_write_after.set(None);
            ram.fail_one_read_after.set(None);
            ram.fail_one_read_burst.set(0);
            ram.fail_next_reads.set(0);
            let Err(save_error) = result else {
                continue;
            };
            let stranded = fs.entry_exists("/NEW.TXT").unwrap();
            assert!(
                !(stranded && matches!(save_error.error, FsOpError::VerifyFailed)),
                "burst shot {shot}: partial target stranded but reported as bare VerifyFailed"
            );
            if matches!(save_error.error, FsOpError::CleanupIncomplete(_)) {
                assert!(
                    stranded,
                    "burst shot {shot}: CleanupIncomplete reported but nothing is listed"
                );
                proven_reported = true;
            }
        }
        assert!(
            proven_reported,
            "no outage exercised the stranded-target report"
        );
    }

    #[test]
    fn save_rollback_never_deletes_a_preexisting_advertised_backup() {
        // A region outage across the staging scan launders all four
        // confirmed probes, so the save reuses ~WIO0000 although a real
        // (previously advertised) ~WIO0000.BAK exists past the parent's
        // first cluster. A save's rollback may then delete the .BAK name
        // while it is ambiguous or resolves to the user's real backup --
        // the bare delete this fix guards. Phase 1 sweeps single-episode
        // outages and requires the pre-existing backup to survive every
        // one; phase 2 pins the guard on the duplicate-entry state itself,
        // which needs a two-episode outage (launder the staging scan, heal
        // for the temporary's staging, launder the backup scan again) that
        // the budget knob cannot produce, so it is constructed directly.
        for budget in 1..=12u32 {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            // Fill the directory's first cluster (16 entries: dot, dotdot,
            // and 14 files) so the staging names' lookups must step the
            // FAT chain past it -- the step a region outage launders.
            for index in 0..14u32 {
                fs.create_empty(&alloc::format!("/DIR/F{index:02}.TXT"))
                    .unwrap();
            }
            // Both land in the directory's second cluster.
            fs.save_verified("/DIR/~WIO0000.BAK", b"pre-existing backup")
                .unwrap();
            fs.save_verified("/DIR/FILE.TXT", b"original contents")
                .unwrap();
            fs.drop_block_cache();
            let geometry = fat32_geometry(131_072, 0).unwrap();
            let fat_start = geometry.partition_start + 32;
            ram.fail_reads_in
                .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
            ram.fail_reads_in_budget.set(Some(budget));
            let result = fs.save_transactional("/DIR/FILE.TXT", b"new contents");
            ram.fail_reads_in.set(None);
            ram.fail_reads_in_budget.set(None);
            // By-name reads resolve to the first-listed entry -- the
            // pre-existing backup -- which must survive every budget with
            // its contents intact.
            assert_eq!(
                fs.read_file("/DIR/~WIO0000.BAK").unwrap(),
                b"pre-existing backup",
                "budget {budget}: the pre-existing backup was mutated"
            );
            let _ = result;
        }
        // Phase 2: two entries bearing the staging name, the pre-existing
        // backup listing first -- the exact configuration the bare
        // rollback delete truncated. Build it by renaming a second file's
        // on-disk entry to the .BAK name.
        let (ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/DIR").unwrap();
        fs.save_verified("/DIR/~WIO0000.BAK", b"pre-existing backup")
            .unwrap();
        fs.save_verified("/DIR/DUP.BIN", b"duplicate shadow")
            .unwrap();
        {
            let mut blocks = ram.blocks.borrow_mut();
            let mut renamed = false;
            for block in blocks.iter_mut() {
                for slot in 0..16 {
                    let offset = slot * 32;
                    if block.contents[offset..offset + 11] == *b"DUP     BIN" {
                        block.contents[offset..offset + 11].copy_from_slice(b"~WIO0000BAK");
                        renamed = true;
                    }
                }
            }
            assert!(renamed, "DUP.BIN's directory entry not found");
        }
        fs.drop_block_cache();
        let bak_entries = || {
            let (page, _) = fs.read_directory_page("/DIR", 0, 32).unwrap();
            page.into_iter()
                .filter(|item| item.name == "~WIO0000.BAK")
                .count()
        };
        assert_eq!(bak_entries(), 2);
        // Every rollback .BAK delete goes through the guard, which must
        // refuse the ambiguous name...
        assert!(
            fs.guarded_rollback_delete("/DIR/~WIO0000.BAK", false) == CleanupOutcome::Blocked,
            "the guard deleted an ambiguous backup name"
        );
        // ...and the post-restore delete resolves the name first, seeing
        // the same ambiguity (anything but a unique file names the backup
        // instead of deleting).
        assert!(matches!(
            fs.resolve_destination_name("/DIR/~WIO0000.BAK"),
            NameResolution::Unsure
        ));
        assert_eq!(
            fs.read_file("/DIR/~WIO0000.BAK").unwrap(),
            b"pre-existing backup"
        );
        assert_eq!(bak_entries(), 2, "a refused delete must not remove entries");
    }

    #[test]
    fn save_rollback_never_deletes_a_preexisting_temporary() {
        // The restore-failure arm deliberately leaves ~WIO*.TMP behind as
        // the only proven copy of that save's new contents. A later
        // save's double-laundered staging scan can reuse the suffix and
        // append a duplicate 8.3 entry, so the rollback's temporary
        // deletes must go through the guard exactly like the backup ones:
        // an ambiguous name strands (tolerated staging debris), never
        // resolves a by-name delete onto the preserved copy. Same
        // construction as the .BAK twin above.
        let (ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/DIR").unwrap();
        fs.save_verified("/DIR/~WIO0000.TMP", b"preserved new contents")
            .unwrap();
        fs.save_verified("/DIR/DUP.BIN", b"duplicate shadow")
            .unwrap();
        {
            let mut blocks = ram.blocks.borrow_mut();
            let mut renamed = false;
            for block in blocks.iter_mut() {
                for slot in 0..16 {
                    let offset = slot * 32;
                    if block.contents[offset..offset + 11] == *b"DUP     BIN" {
                        block.contents[offset..offset + 11].copy_from_slice(b"~WIO0000TMP");
                        renamed = true;
                    }
                }
            }
            assert!(renamed, "DUP.BIN's directory entry not found");
        }
        fs.drop_block_cache();
        let tmp_entries = || {
            let (page, _) = fs.read_directory_page("/DIR", 0, 32).unwrap();
            page.into_iter()
                .filter(|item| item.name == "~WIO0000.TMP")
                .count()
        };
        assert_eq!(tmp_entries(), 2);
        // Every rollback temporary delete goes through the guard, which
        // must refuse the ambiguous name.
        assert!(
            fs.guarded_rollback_delete("/DIR/~WIO0000.TMP", false) == CleanupOutcome::Blocked,
            "the guard deleted an ambiguous temporary name"
        );
        assert_eq!(
            fs.read_file("/DIR/~WIO0000.TMP").unwrap(),
            b"preserved new contents"
        );
        assert_eq!(tmp_entries(), 2, "a refused delete must not remove entries");
    }

    #[test]
    fn successful_save_cleanup_heals_single_read_flakes() {
        // The success path's staging deletes are guarded, and the guard's
        // resolver has no report channel on a save that succeeded: a
        // refusal there would silently strand a full-size staging file,
        // and stranded staging accumulates (each one bumps the next
        // save's suffix) until the card fills. A single transient read
        // flake anywhere in the save must therefore never leave staging
        // debris behind a reported success.
        let total_reads = {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", b"original contents").unwrap();
            let before = ram.reads_seen.get();
            fs.save_transactional("/FILE.TXT", b"new contents").unwrap();
            ram.reads_seen.get() - before
        };
        let mut proven_healed = false;
        for shot in 0..total_reads {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", b"original contents").unwrap();
            ram.fail_one_read_after.set(Some(shot));
            let result = fs.save_transactional("/FILE.TXT", b"new contents");
            let spent = ram.fail_one_read_after.get().is_none();
            ram.fail_one_read_after.set(None);
            if result.is_ok() {
                assert_eq!(fs.read_file("/FILE.TXT").unwrap(), b"new contents");
                let (page, _) = fs.read_directory_page("/", 0, 32).unwrap();
                assert!(
                    page.iter().all(|item| !item.name.starts_with("~WIO")),
                    "shot {shot}: a successful save left staging debris"
                );
                if spent {
                    proven_healed = true;
                }
            }
        }
        assert!(proven_healed, "no shot exercised a healed cleanup flake");
    }

    #[test]
    fn already_exists_after_clean_cleanup_maps_to_copy_collision() {
        let (_ram, fs) = formatted_fs(131_072);
        // The carve-out re-tags a collision after a completed cleanup:
        // nothing bearing the destination name remains, so "already
        // exists" would be false.
        let reported = fs
            .cleanup_partial_destination(FsOpError::AlreadyExists, true, || CleanupOutcome::Clean);
        assert!(matches!(reported, FsOpError::CopyCollision));
        // Without the carve-out the collision passes through untouched
        // (the cleanup never runs; see
        // already_exists_is_never_rolled_back for the file arm's proof).
        let reported = fs
            .cleanup_partial_destination(FsOpError::AlreadyExists, false, || CleanupOutcome::Clean);
        assert!(matches!(reported, FsOpError::AlreadyExists));
        // A blocked cleanup reports the retry-blocking debris, wrapping
        // the original collision.
        let reported = fs.cleanup_partial_destination(FsOpError::AlreadyExists, true, || {
            CleanupOutcome::Blocked
        });
        assert!(matches!(
            reported,
            FsOpError::CleanupIncomplete(inner) if matches!(*inner, FsOpError::AlreadyExists)
        ));
    }

    #[test]
    fn sd_retry_returns_first_success_and_last_failure() {
        let calls = Cell::new(0u32);
        let result: Result<u32, ()> = sd_retry(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(())
            } else {
                Ok(calls.get())
            }
        });
        assert_eq!(result, Ok(3));
        assert_eq!(calls.get(), 3);

        calls.set(0);
        let result: Result<(), u32> = sd_retry(|| {
            calls.set(calls.get() + 1);
            Err(calls.get())
        });
        assert_eq!(result, Err(SD_RETRY_ATTEMPTS as u32));
        assert_eq!(calls.get(), SD_RETRY_ATTEMPTS as u32);

        calls.set(0);
        let result: Result<(), ()> = sd_retry(|| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn delete_survives_any_single_transient_write_failure() {
        let contents = vec![0x33u8; 8192];
        // Twin-measure a clean delete's write-call count (all calls succeed,
        // so writes_seen counts them exactly).
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/DOOMED.TXT", &contents).unwrap();
        let before = ram.writes_seen.get();
        fs.delete_verified("/DOOMED.TXT", false).unwrap();
        let delete_writes = ram.writes_seen.get() - before;
        assert!(delete_writes > 0);
        // One transient write failure at every position must be absorbed:
        // the truncate step swallows its own errors and the entry delete
        // retries under the standard policy.
        for injected in 0..delete_writes {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/DOOMED.TXT", &contents).unwrap();
            ram.fail_one_write_after.set(Some(injected));
            fs.delete_verified("/DOOMED.TXT", false)
                .unwrap_or_else(|error| panic!("delete died at write {injected}: {error:?}"));
            assert!(!fs.entry_exists("/DOOMED.TXT").unwrap());
        }
    }

    #[test]
    fn stuck_delete_after_truncate_reports_delete_incomplete() {
        let contents = vec![0x44u8; 8192];
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/DOOMED.TXT", &contents).unwrap();
        let before = ram.writes_seen.get();
        fs.delete_verified("/DOOMED.TXT", false).unwrap();
        let delete_writes = ram.writes_seen.get() - before;

        // A write failure that latches at the final delete write (the
        // directory-entry delete, after the truncate persisted) must be
        // reported as DeleteIncomplete, not a generic failure implying the
        // contents are intact.
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/DOOMED.TXT", &contents).unwrap();
        ram.fail_writes_after.set(Some(delete_writes - 1));
        let error = fs
            .delete_verified("/DOOMED.TXT", false)
            .expect_err("the latched card cannot finish the delete");
        assert!(matches!(error, FsOpError::DeleteIncomplete(_)));
        assert_eq!(save_failure_reason(&error), "data removed");
        ram.fail_writes.set(false);
        // The honest state: still listed, contents destroyed.
        assert!(fs.entry_exists("/DOOMED.TXT").unwrap());
        assert!(fs.read_file("/DOOMED.TXT").unwrap().is_empty());
    }

    #[test]
    fn recursive_delete_reports_incomplete_after_children_destroyed() {
        fn folder_of_four(sectors: u32) -> (RamBlocks, CardFs<RamBlocks>) {
            let (ram, fs) = formatted_fs(sectors);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..4 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            (ram, fs)
        }
        // Twin-measure a clean folder delete's writes.
        let (ram, fs) = folder_of_four(131_072);
        let before = ram.writes_seen.get();
        fs.delete_verified("/DIR", true).unwrap();
        let folder_writes = ram.writes_seen.get() - before;
        assert!(folder_writes >= 4, "four child deletes plus the folder's");

        // A card that dies mid-way -- after some children's delete writes
        // persisted -- must report DeleteIncomplete, never a refusal
        // implying the folder is intact. Empty files have no clusters to
        // reclaim, so `truncated` is false for every child: this is the
        // FAT16-shaped path where per-file tracking alone would claim
        // nothing was destroyed.
        let (ram, fs) = folder_of_four(131_072);
        ram.fail_writes_after.set(Some(folder_writes / 2));
        let error = fs
            .delete_verified("/DIR", true)
            .expect_err("the latched card cannot finish the folder delete");
        assert!(
            matches!(error, FsOpError::DeleteIncomplete(_)),
            "expected DeleteIncomplete, got {error:?}"
        );
        assert_eq!(save_failure_reason(&error), "data removed");
        ram.fail_writes.set(false);
        // The honest state: the folder survives with only part of its
        // children.
        assert!(fs.entry_exists("/DIR").unwrap());
        let remaining = (0..4)
            .filter(|i| {
                fs.entry_exists(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap()
            })
            .count();
        assert!(remaining < 4, "at least one child was destroyed");
        assert!(remaining > 0, "the latch stopped the delete mid-way");
    }

    #[test]
    fn recursive_delete_of_empty_folders_reports_incomplete() {
        fn folder_of_folders(sectors: u32) -> (RamBlocks, CardFs<RamBlocks>) {
            let (ram, fs) = formatted_fs(sectors);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..4 {
                fs.create_directory_verified(&alloc::format!("/DIR/SUB{i}"))
                    .unwrap();
            }
            (ram, fs)
        }
        // Twin-measure a clean folder delete's writes.
        let (ram, fs) = folder_of_folders(131_072);
        let before = ram.writes_seen.get();
        fs.delete_verified("/DIR", true).unwrap();
        let folder_writes = ram.writes_seen.get() - before;
        assert!(
            folder_writes >= 4,
            "four subfolder deletes plus the folder's"
        );

        // A card that dies after some empty subfolders' entries were
        // removed must report DeleteIncomplete, never a refusal implying
        // the folder is intact. Subfolder entries have no file children to
        // set the per-file flag, so only the entry-removal tracking covers
        // this shape.
        let (ram, fs) = folder_of_folders(131_072);
        ram.fail_writes_after.set(Some(folder_writes / 2));
        let error = fs
            .delete_verified("/DIR", true)
            .expect_err("the latched card cannot finish the folder delete");
        assert!(
            matches!(error, FsOpError::DeleteIncomplete(_)),
            "expected DeleteIncomplete, got {error:?}"
        );
        assert_eq!(save_failure_reason(&error), "data removed");
        ram.fail_writes.set(false);
        // The honest state: the folder survives with only part of its
        // subfolders.
        assert!(fs.entry_exists("/DIR").unwrap());
        let remaining = (0..4)
            .filter(|i| fs.entry_exists(&alloc::format!("/DIR/SUB{i}")).unwrap())
            .count();
        assert!(remaining < 4, "at least one subfolder was destroyed");
        assert!(remaining > 0, "the latch stopped the delete mid-way");
    }

    #[test]
    fn already_exists_never_triggers_destructive_cleanup() {
        // AlreadyExists proves the failed copy created nothing: the
        // blocking entry pre-existed (reachable only through a
        // double-laundered absence pre-check), so running the rollback
        // would delete the user's pre-existing destination, not this
        // operation's debris.
        let (_ram, fs) = formatted_fs(131_072);
        fs.save_verified("/KEEP.TXT", b"precious").unwrap();
        let (assessed, rollback) = fs.assess_failure(FsOpError::AlreadyExists);
        assert!(matches!(assessed, FsOpError::AlreadyExists));
        assert!(!rollback, "AlreadyExists must never enable rollback");
        let reported = fs.cleanup_partial_destination(FsOpError::AlreadyExists, false, || {
            let _ = fs.delete_file_at("/KEEP.TXT");
            CleanupOutcome::Clean
        });
        assert!(
            matches!(reported, FsOpError::AlreadyExists),
            "the error must pass through unwrapped, got {reported:?}"
        );
        assert_eq!(fs.read_file("/KEEP.TXT").unwrap(), b"precious");
    }

    #[test]
    fn delete_never_reports_not_found_while_the_entry_exists() {
        // The manager's directory walks discard FAT chain-read failures and
        // fall through to NotFound, so a transient read flake during a
        // delete can claim an existing entry is missing. 512-byte clusters
        // (one sector per cluster on this image) hold 16 directory entries;
        // 20 files plus the dot entries span two clusters, so deleting a
        // file in the second cluster crosses the launder-prone chain read.
        // Injection is scoped to delete_entry_checked via a pre-opened
        // handle: the path-walk helpers launder independently and are out
        // of this test's scope.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..20 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            (ram, fs)
        };

        // Twin-measure the reads a clean scoped delete consumes.
        let (ram, fs) = build();
        let dir = fs.open_dir_path("/DIR").unwrap();
        let before = ram.reads_seen.get();
        fs.delete_entry_checked(dir, "F17.TXT", false).unwrap();
        let clean_reads = ram.reads_seen.get() - before;
        let _ = fs.mgr.close_dir(dir);
        assert!(clean_reads > 0);

        for injected in 0..clean_reads {
            let (ram, fs) = build();
            let dir = fs.open_dir_path("/DIR").unwrap();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.delete_entry_checked(dir, "F17.TXT", false);
            ram.fail_one_read_after.set(None);
            let _ = fs.mgr.close_dir(dir);
            let exists = fs.entry_exists("/DIR/F17.TXT").unwrap();
            match result {
                Ok(()) => assert!(!exists, "Ok but entry survives at read {injected}"),
                Err(error) => {
                    assert!(exists, "error {error:?} but entry gone at read {injected}");
                    assert!(
                        !matches!(error, FsOpError::Fs(embedded_sdmmc::Error::NotFound)),
                        "laundered NotFound surfaced at read {injected}"
                    );
                }
            }
        }
    }

    #[test]
    fn create_directory_survives_any_single_transient_read_flake() {
        // The post-create verify must confirm a NotFound before reporting
        // it: a single laundered read would otherwise turn a just-created
        // directory into "file not found". 20 files plus the dot entries
        // span two clusters, so lookups of the new entry cross the
        // launder-prone chain read.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..20 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            (ram, fs)
        };
        // Twin-measure the reads a clean create consumes.
        let (ram, fs) = build();
        let before = ram.reads_seen.get();
        fs.create_directory_verified("/DIR/NEWDIR").unwrap();
        let clean_reads = ram.reads_seen.get() - before;
        assert!(clean_reads > 0);

        for injected in 0..clean_reads {
            let (ram, fs) = build();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.create_directory_verified("/DIR/NEWDIR");
            ram.fail_one_read_after.set(None);
            assert!(
                !matches!(result, Err(FsOpError::Fs(embedded_sdmmc::Error::NotFound))),
                "read {injected}: a just-created directory reported as not found"
            );
            if result.is_ok() {
                assert!(fs.entry_exists("/DIR/NEWDIR").unwrap());
            }
        }
    }

    #[test]
    fn confirmed_absence_after_truncate_reports_delete_incomplete() {
        // The double-laundered residue: after the truncate persisted, a FAT
        // region that stops answering makes the delete's directory walk
        // launder its chain-step read into NotFound, and the confirming
        // walk launders identically against the same dead sectors. The
        // entry provably existed moments ago (the truncate opened it), so
        // the honest report is DeleteIncomplete -- never "file not found"
        // for a listed, emptied entry.
        let (ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/DIR").unwrap();
        for i in 0..20 {
            fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                .unwrap();
        }
        // Four clusters of content, and an entry in the directory's second
        // cluster so lookups must step the FAT chain.
        fs.save_verified("/DIR/F17.TXT", &[0x11u8; 2048]).unwrap();

        let dir = fs.open_dir_path("/DIR").unwrap();
        assert!(fs.reclaim_file_clusters_in_dir(dir, "F17.TXT"));
        // The truncate may have left its blocks cached; the failure must
        // come from the card, not be masked by an echo.
        fs.drop_block_cache();
        let geometry = fat32_geometry(131_072, 0).unwrap();
        let fat_start = geometry.partition_start + 32;
        ram.fail_reads_in
            .set(Some((fat_start, fat_start + 2 * geometry.fat_sectors)));
        let error = fs
            .delete_entry_checked(dir, "F17.TXT", true)
            .expect_err("the dead FAT region cannot confirm the delete");
        ram.fail_reads_in.set(None);
        let _ = fs.mgr.close_dir(dir);
        assert!(
            matches!(error, FsOpError::DeleteIncomplete(_)),
            "expected DeleteIncomplete, got {error:?}"
        );
        assert_eq!(save_failure_reason(&error), "data removed");
        // The honest state: still listed, contents destroyed.
        assert!(fs.entry_exists("/DIR/F17.TXT").unwrap());
        assert!(fs.read_file("/DIR/F17.TXT").unwrap().is_empty());
    }

    #[test]
    fn move_reports_source_ghost_when_its_delete_sticks() {
        let contents = vec![0x77u8; 8192];
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/SRC.TXT", &contents).unwrap();
        let before = ram.writes_seen.get();
        fs.move_entry_verified("/SRC.TXT", "/DST.TXT").unwrap();
        let move_writes = ram.writes_seen.get() - before;

        // The move's final write is the source's directory-entry delete; a
        // failure latching there leaves a verified destination plus an
        // emptied source ghost, and the caller must hear that -- "Move
        // failed" would invite deleting the only good copy.
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/SRC.TXT", &contents).unwrap();
        ram.fail_writes_after.set(Some(move_writes - 1));
        let error = fs
            .move_entry_verified("/SRC.TXT", "/DST.TXT")
            .expect_err("the latched card cannot finish the move");
        assert!(matches!(error, FsOpError::DeleteIncomplete(_)));
        ram.fail_writes.set(false);
        assert_eq!(fs.read_file("/DST.TXT").unwrap(), contents);
        assert!(fs.entry_exists("/SRC.TXT").unwrap());
        assert!(fs.read_file("/SRC.TXT").unwrap().is_empty());
    }

    #[test]
    fn fat16_move_source_delete_failure_reports_source_remains() {
        // FAT16 never truncates before a delete (reclaim is FAT32-only), so
        // DeleteIncomplete is unreachable there: a stuck source delete after
        // a fully verified copy must still surface as success-with-caveat
        // (SourceRemains), not a raw "Move failed" that invites deleting
        // the only good copy -- with the retry then dead-ending at
        // "already exists".
        let contents = vec![0x77u8; 2048];
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/SRC.TXT", &contents).unwrap();
        let before = ram.writes_seen.get();
        fs.move_entry_verified("/SRC.TXT", "/DST.TXT").unwrap();
        let move_writes = ram.writes_seen.get() - before;

        // Twin fixture with the identical write history: latch the failure
        // at the move's final write, the source's directory-entry delete.
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        fs.save_verified("/SRC.TXT", &contents).unwrap();
        ram.fail_writes_after.set(Some(move_writes - 1));
        let error = fs
            .move_entry_verified("/SRC.TXT", "/DST.TXT")
            .expect_err("the latched card cannot finish the move");
        assert!(
            matches!(error, FsOpError::SourceRemains(_)),
            "expected SourceRemains, got {error:?}"
        );
        assert_eq!(save_failure_reason(&error), "old entry remains");
        ram.fail_writes.set(false);
        // The truth SourceRemains reports: destination verified, source
        // still intact (no truncate ran).
        assert_eq!(fs.read_file("/DST.TXT").unwrap(), contents);
        assert_eq!(fs.read_file("/SRC.TXT").unwrap(), contents);
        // Without the caveat report, the user's retry would hit this
        // unexplained dead end.
        assert!(matches!(
            fs.move_entry_verified("/SRC.TXT", "/DST.TXT"),
            Err(FsOpError::AlreadyExists)
        ));
    }

    #[test]
    fn masked_extension_fault_reports_card_fault_and_preserves_staging() {
        let original = vec![0x55u8; 8192];
        let new_contents = vec![0xaau8; 8192];
        let staging = staging_write_count(&new_contents, &original);
        // Sweep total card death across every write of the commit step. The
        // manager masks device faults met while extending the target's
        // cluster chain as DiskFull; the save must never report a dead card
        // as "storage is full", and must leave the staging copies in place.
        let mut saw_unresponsive = false;
        let mut control_run_seen = false;
        for extra in 0..10_000u32 {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/FILE.TXT", &original).unwrap();
            ram.die_after_writes.set(Some(staging + extra));
            let result = fs.save_transactional("/FILE.TXT", &new_contents);
            let died = ram.die_after_writes.get().is_none();
            ram.fail_writes.set(false);
            ram.fail_reads.set(false);
            if !died {
                // The death point fell beyond the whole operation.
                assert!(result.is_ok());
                control_run_seen = true;
                break;
            }
            let Err(error) = result else {
                // Death landed in the post-commit staging cleanup, which is
                // deliberately swallowed; the save itself already won.
                continue;
            };
            assert_ne!(
                save_failure_reason(&error.error),
                "storage is full",
                "dead card misread as full at commit write {extra}"
            );
            if matches!(error.error, FsOpError::DeviceUnresponsive) {
                saw_unresponsive = true;
            }
            // No rollback ran through the dead card: both staging copies
            // survive as the recovery data, and the caller is told about
            // the backup.
            assert!(error.backup_kept.is_some());
            assert_eq!(fs.read_file("/~WIO0000.TMP").unwrap(), new_contents);
            assert_eq!(fs.read_file("/~WIO0000.BAK").unwrap(), original);
        }
        assert!(control_run_seen);
        assert!(
            saw_unresponsive,
            "no death point exercised the DiskFull mask"
        );
    }

    #[test]
    fn genuine_disk_full_rolls_back_and_reports_full() {
        // FAT16: the manager keeps no free-cluster counter there, so a
        // genuine exhaustion surfaces cleanly. (Exhausting FAT32 trips the
        // manager's own FSInfo counter underflow in debug builds.)
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        let free = fs.free_space_bytes().unwrap();
        // Fill the card down to eight clusters, then ask a save to stage a
        // document that cannot fit: a genuinely full, healthy card must keep
        // the honest "storage is full" report and roll its staging back.
        let filler = vec![0x11u8; (free - 8 * 512) as usize];
        fs.save_verified("/FILLER.BIN", &filler).unwrap();
        fs.save_verified("/FILE.TXT", b"small").unwrap();
        let error = fs
            .save_transactional("/FILE.TXT", &vec![0x22u8; 16 * 1024])
            .expect_err("the full card cannot stage the new contents");
        assert!(
            matches!(
                error.error,
                FsOpError::Fs(
                    embedded_sdmmc::Error::DiskFull | embedded_sdmmc::Error::NotEnoughSpace
                )
            ),
            "expected a full-disk report, got {error:?}"
        );
        assert_eq!(save_failure_reason(&error.error), "storage is full");
        // The failure hit the staging stage, before any backup existed.
        assert!(error.backup_kept.is_none());
        assert!(staging_leftovers(&fs).is_empty());
        assert_eq!(fs.read_file("/FILE.TXT").unwrap(), b"small");
    }

    #[test]
    fn move_into_high_byte_folder_does_not_panic() {
        let (_ram, fs) = formatted_fs(131_072);
        // "É" (U+00C9) is a storable 8.3 byte that Display re-renders as two
        // UTF-8 bytes; "/AB".len() == 3 lands inside it in "/XÉ/AB", which a
        // str slice in path_is_inside panicked on.
        fs.create_directory_verified("/X\u{c9}").unwrap();
        fs.create_directory_verified("/AB").unwrap();
        fs.move_entry_verified("/AB", "/X\u{c9}/AB").unwrap();
        assert!(fs.entry_exists("/X\u{c9}/AB").unwrap());
        assert!(!fs.entry_exists("/AB").unwrap());
    }

    #[test]
    fn probe_and_mount_agree_on_mbr_slot_acceptance() {
        // probe_fat mirrors embedded-sdmmc's private MBR status mask and
        // partition-type accept list; a slot the probe accepts but the mount
        // refuses would dead-end a good card at the format prompt. Prove the
        // two layers agree over one valid FAT32 image with every status and
        // type combination patched into slot 0.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let statuses = [0x00u8, 0x80, 0x01, 0x7f, 0xff];
        // Derived from the real accept lists so a constant gaining an entry
        // is automatically exercised here; only the unknown types are local.
        let kinds: Vec<u8> = MOUNTABLE_PARTITION_TYPES
            .into_iter()
            .chain(KNOWN_UNSUPPORTED_PARTITION_TYPES)
            .chain([0x00, 0x83])
            .collect();
        for status in statuses {
            for &kind in &kinds {
                {
                    let mut blocks = ram.blocks.borrow_mut();
                    blocks[0].contents[446] = status;
                    blocks[0].contents[446 + 4] = kind;
                }
                let probed = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors);
                let mounted =
                    VolumeManager::new(ram.clone(), FixedTimeSource).open_raw_volume(VolumeIdx(0));
                assert_eq!(
                    probed.is_ok(),
                    mounted.is_ok(),
                    "probe/mount disagree for status {status:#04x} kind {kind:#04x}"
                );
                if let Ok(layout) = probed {
                    assert_eq!(layout.partition_index, 0);
                }
            }
        }
    }

    /// The on-disk 11-byte form of an 8.3 name (blank-padded, uppercase).
    fn sfn_bytes(name: &str) -> [u8; 11] {
        let mut out = [b' '; 11];
        let (base, extension) = match name.rfind('.') {
            Some(dot) => (&name[..dot], &name[dot + 1..]),
            None => (name, ""),
        };
        for (index, byte) in base.bytes().enumerate() {
            out[index] = byte.to_ascii_uppercase();
        }
        for (index, byte) in extension.bytes().enumerate() {
            out[8 + index] = byte.to_ascii_uppercase();
        }
        out
    }

    /// Set the read-only bit on the directory entry named `name`, scanning
    /// the raw sectors -- what a PC would have done, since the device can
    /// never write the bit itself. The name must be unique on the volume.
    fn patch_read_only(ram: &RamBlocks, name: &str) {
        let pattern = sfn_bytes(name);
        let mut patched = 0;
        for block in ram.blocks.borrow_mut().iter_mut() {
            for entry in 0..16 {
                let start = entry * 32;
                if block.contents[start..start + 11] == pattern {
                    block.contents[start + 11] |= 0x01;
                    patched += 1;
                }
            }
        }
        assert_eq!(patched, 1, "expected exactly one entry named {name}");
    }

    #[test]
    fn read_only_entries_refuse_save_delete_and_move() {
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/ROFILE.TXT", b"protected contents")
            .unwrap();
        fs.create_directory_verified("/KEEP").unwrap();
        fs.create_directory_verified("/KEEP/INNER").unwrap();
        fs.save_verified("/KEEP/SIBLING.TXT", b"sibling data")
            .unwrap();
        fs.save_verified("/KEEP/INNER/LOCKED.TXT", b"deep protected")
            .unwrap();
        drop(fs);
        patch_read_only(&ram, "ROFILE.TXT");
        patch_read_only(&ram, "LOCKED.TXT");
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), 131_072).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();

        // Direct operations on a read-only file are refused; the emulated
        // move would otherwise delete the protected original and recreate it
        // writable.
        assert!(matches!(
            fs.save_transactional("/ROFILE.TXT", b"overwrite"),
            Err(SaveError {
                error: FsOpError::Fs(embedded_sdmmc::Error::ReadOnly),
                ..
            })
        ));
        assert!(matches!(
            fs.delete_verified("/ROFILE.TXT", false),
            Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly))
        ));
        assert!(matches!(
            fs.move_entry_verified("/ROFILE.TXT", "/RENAMED.TXT"),
            Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly))
        ));
        assert_eq!(fs.read_file("/ROFILE.TXT").unwrap(), b"protected contents");
        assert!(!fs.entry_exists("/RENAMED.TXT").unwrap());

        // A folder holding a read-only entry deep inside is refused by the
        // pre-scan before anything at all is deleted or copied.
        assert!(matches!(
            fs.delete_verified("/KEEP", true),
            Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly))
        ));
        assert!(matches!(
            fs.move_entry_verified("/KEEP", "/MOVED"),
            Err(FsOpError::Fs(embedded_sdmmc::Error::ReadOnly))
        ));
        assert_eq!(fs.read_file("/KEEP/SIBLING.TXT").unwrap(), b"sibling data");
        assert_eq!(
            fs.read_file("/KEEP/INNER/LOCKED.TXT").unwrap(),
            b"deep protected"
        );
        assert!(!fs.entry_exists("/MOVED").unwrap());
    }

    #[test]
    fn save_confirms_target_presence_before_destroying_it() {
        // One laundered NotFound during save_transactional's existence
        // check must not skip the pre-checks and the backup staging: with
        // existed=false the in-place rewrite would meet the read-only
        // refusal only at the commit, and the !existed rollback would then
        // delete the protected original (delete_entry_in_dir never checks
        // the read-only bit) -- the only copy, with no backup kept.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..20 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            fs.save_verified("/DIR/F17.TXT", b"protected contents")
                .unwrap();
            drop(fs);
            patch_read_only(&ram, "F17.TXT");
            let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), 131_072).unwrap();
            let fs = CardFs::mount(ram.clone(), layout).unwrap();
            (ram, fs)
        };

        // Twin-measure the reads the existence lookup consumes.
        let (ram, fs) = build();
        let before = ram.reads_seen.get();
        fs.find_entry("/DIR/F17.TXT").unwrap();
        let lookup_reads = ram.reads_seen.get() - before;
        assert!(lookup_reads > 0);

        for injected in 0..lookup_reads {
            let (ram, fs) = build();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.save_transactional("/DIR/F17.TXT", b"overwrite");
            ram.fail_one_read_after.set(None);
            assert!(result.is_err(), "save must not succeed at read {injected}");
            assert!(
                fs.entry_exists("/DIR/F17.TXT").unwrap(),
                "original entry lost at read {injected}"
            );
            assert_eq!(
                fs.read_file("/DIR/F17.TXT").unwrap(),
                b"protected contents",
                "original contents lost at read {injected}"
            );
        }
    }

    #[test]
    fn save_confirms_staging_name_before_reuse() {
        // The staging-name scan acts destructively on absence too: a
        // laundered NotFound on an existing ~WIO0000.TMP would reuse the
        // suffix, and save_verified's CreateOrTruncate open would destroy a
        // prior failed save's only proven copy of the new contents.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..20 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            fs.save_verified("/DIR/F05.TXT", b"old contents").unwrap();
            fs.save_verified("/DIR/~WIO0000.TMP", b"precious recovery data")
                .unwrap();
            (ram, fs)
        };

        // Twin-measure the pre-staging lookup window: target existence,
        // the occupied suffix, and the free suffix the scan settles on.
        let (ram, fs) = build();
        let before = ram.reads_seen.get();
        fs.find_entry("/DIR/F05.TXT").unwrap();
        assert!(fs.entry_exists("/DIR/~WIO0000.TMP").unwrap());
        assert!(!fs.entry_exists("/DIR/~WIO0001.TMP").unwrap());
        assert!(!fs.entry_exists("/DIR/~WIO0001.BAK").unwrap());
        let lookup_reads = ram.reads_seen.get() - before;
        assert!(lookup_reads > 0);

        for injected in 0..lookup_reads {
            let (ram, fs) = build();
            ram.fail_one_read_after.set(Some(injected));
            let _ = fs.save_transactional("/DIR/F05.TXT", b"new contents");
            ram.fail_one_read_after.set(None);
            assert_eq!(
                fs.read_file("/DIR/~WIO0000.TMP").unwrap(),
                b"precious recovery data",
                "staged recovery data destroyed at read {injected}"
            );
        }
    }

    #[test]
    fn move_confirms_destination_before_clobbering_it() {
        // A laundered NotFound on the move's destination pre-check lets the
        // copy run; the copy then fails AlreadyExists, and its rollback
        // would delete the *pre-existing* destination file.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            for i in 0..20 {
                fs.create_empty(&alloc::format!("/DIR/F{i:02}.TXT"))
                    .unwrap();
            }
            fs.save_verified("/DIR/F17.TXT", b"existing destination")
                .unwrap();
            fs.save_verified("/SRC.TXT", b"source data").unwrap();
            (ram, fs)
        };

        // Twin-measure the two pre-check lookups.
        let (ram, fs) = build();
        let before = ram.reads_seen.get();
        fs.find_entry("/SRC.TXT").unwrap();
        fs.find_entry("/DIR/F17.TXT").unwrap();
        let lookup_reads = ram.reads_seen.get() - before;
        assert!(lookup_reads > 0);

        for injected in 0..lookup_reads {
            let (ram, fs) = build();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.move_entry_verified("/SRC.TXT", "/DIR/F17.TXT");
            ram.fail_one_read_after.set(None);
            assert!(result.is_err(), "move must not succeed at read {injected}");
            assert_eq!(
                fs.read_file("/DIR/F17.TXT").unwrap(),
                b"existing destination",
                "destination clobbered at read {injected}"
            );
            assert_eq!(
                fs.read_file("/SRC.TXT").unwrap(),
                b"source data",
                "source damaged at read {injected}"
            );
        }
    }

    #[test]
    fn move_verify_flake_never_fails_a_landed_move() {
        // Once the copy is verified and the source delete confirmed, a
        // transient read during the post-move verification must never turn
        // the landed move into a reported failure -- "file not found" for
        // a move that succeeded invites deleting the only good copy.
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/DIR").unwrap();
            fs.save_verified("/SRC.TXT", b"source data").unwrap();
            (ram, fs)
        };
        // Twin-measure a clean move's reads.
        let (ram, fs) = build();
        let before = ram.reads_seen.get();
        fs.move_entry_verified("/SRC.TXT", "/DIR/DST.TXT").unwrap();
        let move_reads = ram.reads_seen.get() - before;
        assert!(move_reads > 0);

        for injected in 0..move_reads {
            let (ram, fs) = build();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.move_entry_verified("/SRC.TXT", "/DIR/DST.TXT");
            ram.fail_one_read_after.set(None);
            assert!(
                !matches!(result, Err(FsOpError::Fs(embedded_sdmmc::Error::NotFound))),
                "move reported 'file not found' at read {injected}"
            );
            let landed = fs
                .read_file("/DIR/DST.TXT")
                .map(|data| data == b"source data")
                .unwrap_or(false)
                && !fs.entry_exists("/SRC.TXT").unwrap();
            if landed {
                assert!(
                    result.is_ok(),
                    "landed move reported {result:?} at read {injected}"
                );
            }
        }
    }

    #[test]
    fn delete_verify_flake_never_reports_not_deleted() {
        // Once delete_entry_checked has confirmed the removal, a transient
        // read during delete_verified's advisory re-check must not convert
        // the completed delete into a "Not deleted" report.
        let contents = vec![0x55u8; 4096];
        let (ram, fs) = formatted_fs(131_072);
        fs.save_verified("/DOOMED.TXT", &contents).unwrap();
        let before = ram.reads_seen.get();
        fs.delete_verified("/DOOMED.TXT", false).unwrap();
        let delete_reads = ram.reads_seen.get() - before;
        assert!(delete_reads > 0);

        for injected in 0..delete_reads {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/DOOMED.TXT", &contents).unwrap();
            ram.fail_one_read_after.set(Some(injected));
            let result = fs.delete_verified("/DOOMED.TXT", false);
            ram.fail_one_read_after.set(None);
            if !fs.entry_exists("/DOOMED.TXT").unwrap() {
                assert!(
                    result.is_ok(),
                    "completed delete reported {result:?} at read {injected}"
                );
            }
        }
    }

    #[test]
    fn format_marks_phantom_fat_tail_entries_bad() {
        // The manager's free-cluster search scans whole FAT sectors without
        // re-checking its end bound, so zeroed spare entries in the FAT's
        // last sector would be handed out as allocatable clusters mapping
        // past the end of the partition once the card fills up.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let phantom_offset = (fat_entry_bytes(geometry.cluster_count, true) % 512) as usize;
        assert_ne!(phantom_offset, 0, "fixture must have a phantom tail");

        let first_fat = geometry.partition_start + 32;
        let blocks = ram.blocks.borrow();
        for copy in 0..2u32 {
            let last = (first_fat + (copy + 1) * geometry.fat_sectors - 1) as usize;
            let contents = &blocks[last].contents;
            assert!(
                contents[..phantom_offset].iter().all(|byte| *byte == 0),
                "FAT copy {copy}: real entries below the tail must stay free"
            );
            for entry in contents[phantom_offset..].chunks_exact(4) {
                assert_eq!(
                    entry,
                    [0xf7, 0xff, 0xff, 0x0f],
                    "FAT copy {copy}: phantom tail entry not marked bad"
                );
            }
        }
        drop(blocks);

        // The volume still mounts and reports the FSInfo figure, which
        // never counted the phantoms.
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(
            fs.free_space_bytes(),
            Some(
                u64::from(geometry.cluster_count - 1)
                    * u64::from(geometry.sectors_per_cluster)
                    * 512
            )
        );
    }

    #[test]
    fn mount_repairs_zeroed_phantom_fat_tail() {
        // Foreign formatters commonly leave the FAT tail zeroed, exposing
        // the same phantom-cluster hazard the formatter guards against;
        // mount must apply the same bad-cluster marks.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let phantom_offset = (fat_entry_bytes(geometry.cluster_count, true) % 512) as usize;
        assert_ne!(phantom_offset, 0, "fixture must have a phantom tail");

        // Simulate the foreign card: zero the tail entries of both copies.
        let first_fat = geometry.partition_start + 32;
        let tail_sectors: [usize; 2] = core::array::from_fn(|copy| {
            (first_fat + (copy as u32 + 1) * geometry.fat_sectors - 1) as usize
        });
        {
            let mut blocks = ram.blocks.borrow_mut();
            for last in tail_sectors {
                blocks[last].contents[phantom_offset..].fill(0);
            }
        }

        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let _fs = CardFs::mount(ram.clone(), layout).unwrap();

        let blocks = ram.blocks.borrow();
        for (copy, last) in tail_sectors.into_iter().enumerate() {
            let contents = &blocks[last].contents;
            assert!(
                contents[..phantom_offset].iter().all(|byte| *byte == 0),
                "FAT copy {copy}: real entries below the tail must stay free"
            );
            for entry in contents[phantom_offset..].chunks_exact(4) {
                assert_eq!(
                    entry,
                    [0xf7, 0xff, 0xff, 0x0f],
                    "FAT copy {copy}: phantom tail entry not re-marked at mount"
                );
            }
        }
    }

    #[test]
    fn mount_writes_nothing_to_a_healthy_fat() {
        // A card whose phantom tail is already marked (any card this
        // firmware formatted) must mount without a single write.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let before = ram.writes_seen.get();
        let _fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(ram.writes_seen.get(), before, "mount must not write");
    }

    #[test]
    fn under_provisioned_fat32_is_rejected_outright() {
        // A single corrupted FATSz32 field yields a BPB whose declared FAT
        // cannot hold every derived cluster's entry, so the manager's
        // allocator would map high clusters past the FAT into the root
        // directory or user data. The parser refuses such a volume: the
        // probe reports it invalid (routing the card to the format prompt
        // instead of ever mounting it), and even a direct mount with a
        // stale layout must not write.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let doctored = geometry.fat_sectors / 2;
        {
            let mut blocks = ram.blocks.borrow_mut();
            for bpb in [geometry.partition_start, geometry.partition_start + 6] {
                put_u32(&mut blocks[bpb as usize].contents[36..40], doctored);
            }
        }
        let boot = read_device_sector(&ram, layout.start_lba).unwrap();
        assert!(matches!(parse_fat_boot_sector(&boot), BootSector::Invalid));
        assert!(matches!(
            probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors),
            Err(ProbeError::Invalid)
        ));
        let before = ram.writes_seen.get();
        // The manager may or may not still open the doctored volume; only
        // the absence of writes matters.
        let _ = CardFs::mount(ram.clone(), layout);
        assert_eq!(ram.writes_seen.get(), before, "mount must not write");
    }

    #[test]
    fn oversized_fat32_cluster_count_is_rejected() {
        // FAT32 cluster numbers top out at 0x0fff_fff6, so a BPB deriving
        // a cluster count past 0x0fff_fff5 is out of spec (and would
        // overflow the consumers' entry arithmetic); the parser refuses it.
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let mut boot = read_device_sector(&ram, geometry.partition_start).unwrap();
        put_u32(&mut boot[32..36], u32::MAX);
        assert!(matches!(parse_fat_boot_sector(&boot), BootSector::Invalid));
    }

    #[test]
    fn mount_repairs_zeroed_phantom_fat16_tail() {
        // Foreign FAT16 formatters commonly leave the FAT tail zeroed, and
        // the manager's FAT16 free-cluster scan has the same whole-sector
        // overrun as FAT32's; mount must apply the same bad-cluster marks.
        let (ram, layout) = fat16_media_custom(16, false);
        let _fs = CardFs::mount(ram.clone(), layout).unwrap();
        let first_phantom = (fat_entry_bytes(FAT16_CLUSTERS, false) % 512) as usize;
        {
            let blocks = ram.blocks.borrow();
            for copy in 0..2u32 {
                let last_fat = (FAT16_START + FAT16_RESERVED + (copy + 1) * 16 - 1) as usize;
                let contents = &blocks[last_fat].contents;
                assert!(
                    contents[..first_phantom].iter().all(|byte| *byte == 0),
                    "FAT copy {copy}: real entries below the tail must stay free"
                );
                for entry in contents[first_phantom..].chunks_exact(2) {
                    assert_eq!(
                        entry,
                        [0xf7, 0xff],
                        "FAT copy {copy}: phantom tail entry not marked at mount"
                    );
                }
            }
        }
        // Idempotent: the repaired card is now the healthy case.
        let before = ram.writes_seen.get();
        let _fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(ram.writes_seen.get(), before, "second mount must not write");
    }

    #[test]
    fn mount_writes_nothing_to_a_healthy_fat16() {
        // The FAT16 twin of the FAT32 assertion above: an already-marked
        // tail must mount without a single write.
        let (ram, layout) = fat16_media();
        let before = ram.writes_seen.get();
        let _fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(ram.writes_seen.get(), before, "mount must not write");
    }

    #[test]
    fn under_provisioned_fat16_is_rejected_outright() {
        // Eight FAT sectors cannot hold 4,087 two-byte entries; such a
        // volume is inconsistent beyond any repair's remit, so the parser
        // refuses it and the probe reports the card invalid (routing it to
        // the format prompt) without a single write.
        let (ram, media_sectors) = fat16_media_image(8, false);
        let boot = read_device_sector(&ram, FAT16_START).unwrap();
        assert!(matches!(parse_fat_boot_sector(&boot), BootSector::Invalid));
        let before = ram.writes_seen.get();
        assert!(matches!(
            probe_fat(&mut SdStream::new(ram.clone()).unwrap(), media_sectors),
            Err(ProbeError::Invalid)
        ));
        assert_eq!(ram.writes_seen.get(), before, "probe must not write");
    }

    #[test]
    fn failed_folder_move_removes_partial_destination() {
        let contents_a: Vec<u8> = (0..4096u32).map(|i| i as u8).collect();
        let contents_b: Vec<u8> = (0..4096u32).map(|i| (i * 7) as u8).collect();
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.create_directory_verified("/SRC").unwrap();
            fs.save_verified("/SRC/A.TXT", &contents_a).unwrap();
            fs.save_verified("/SRC/B.TXT", &contents_b).unwrap();
            (ram, fs)
        };
        // Total writes of a successful move, measured on a twin with an
        // identical write history, bound the sweep and aim the companion.
        let (ram, fs) = build();
        let before = ram.writes_seen.get();
        fs.move_entry_verified("/SRC", "/DEST").unwrap();
        let move_writes = ram.writes_seen.get() - before;

        // Sweep a single-write corruption across the whole move. Every hit
        // on a copied file's data fails the copy verification and must roll
        // the partial destination back so a retry is not refused as
        // AlreadyExists; corruption of unrelated metadata may fail
        // differently, but a missing rollback would leave /DEST behind on
        // every data hit and score zero below.
        let mut cleaned = 0;
        for injected in 0..move_writes {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(injected));
            let Err(error) = fs.move_entry_verified("/SRC", "/DEST") else {
                continue;
            };
            assert!(!error.is_device_error());
            if !fs.entry_exists("/DEST").unwrap_or(true)
                && fs
                    .read_file("/SRC/A.TXT")
                    .is_ok_and(|bytes| bytes == contents_a)
                && fs
                    .read_file("/SRC/B.TXT")
                    .is_ok_and(|bytes| bytes == contents_b)
            {
                cleaned += 1;
            }
        }
        assert!(cleaned >= 4, "only {cleaned} failures rolled back cleanly");

        // Companion: a card whose writes die and stay dead. Not every fail
        // point surfaces as a device error (the manager reports hard
        // failures during cluster allocation as DiskFull; reads stay alive
        // here, so assess_failure's probe passes and those points keep
        // rollback enabled -- its failure then surfaces as
        // CleanupIncomplete rather than being swallowed), so scan for
        // the first one that does -- it lies in the copy phase, before any
        // source deletion -- and prove the move stopped with the source
        // intact instead of mutating further through a dead card.
        let mut device_error_proven = false;
        for injected in 0..move_writes {
            let (ram, fs) = build();
            ram.fail_writes_after.set(Some(injected));
            let result = fs.move_entry_verified("/SRC", "/DEST");
            ram.fail_writes.set(false);
            if result.as_ref().is_err_and(FsOpError::is_device_error) {
                assert_eq!(fs.read_file("/SRC/A.TXT").unwrap(), contents_a);
                assert_eq!(fs.read_file("/SRC/B.TXT").unwrap(), contents_b);
                device_error_proven = true;
                break;
            }
        }
        assert!(device_error_proven);
    }

    #[test]
    fn stuck_rollback_surfaces_cleanup_incomplete() {
        let contents = vec![0x5au8; 4096];
        let build = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.save_verified("/SRC.TXT", &contents).unwrap();
            (ram, fs)
        };
        let (ram, fs) = build();
        let before = ram.writes_seen.get();
        fs.move_entry_verified("/SRC.TXT", "/DST.TXT").unwrap();
        let move_writes = ram.writes_seen.get() - before;

        // Corrupt one write (a non-device failure that keeps rollback
        // enabled), then latch every later write dead: the rollback delete
        // of the partial destination cannot finish, and the ghost it leaves
        // must be reported instead of swallowed. Scan for the first
        // injection point that lands this way -- a hit on copied data fails
        // the copy verification with reads still alive.
        let mut proven = false;
        for injected in 0..move_writes {
            let (ram, fs) = build();
            ram.corrupt_one_write_after.set(Some(injected));
            ram.fail_writes_after.set(Some(injected + 1));
            let result = fs.move_entry_verified("/SRC.TXT", "/DST.TXT");
            ram.corrupt_one_write_after.set(None);
            ram.fail_writes_after.set(None);
            ram.fail_writes.set(false);
            if let Err(error @ FsOpError::CleanupIncomplete(_)) = result {
                assert_eq!(save_failure_reason(&error), "partial copy left");
                // The honest state: destination ghost listed, source intact.
                assert!(fs.entry_exists("/DST.TXT").unwrap());
                assert_eq!(fs.read_file("/SRC.TXT").unwrap(), contents);
                // The ghost blocks a retry until the user deletes it --
                // exactly what the error message warns about.
                assert!(matches!(
                    fs.move_entry_verified("/SRC.TXT", "/DST.TXT"),
                    Err(FsOpError::AlreadyExists)
                ));
                proven = true;
                break;
            }
        }
        assert!(proven, "no injection point produced CleanupIncomplete");
    }

    #[test]
    fn large_folder_deletes_and_moves_within_bounds() {
        let (_ram, fs) = formatted_fs(131_072);
        fs.create_directory_verified("/BIG").unwrap();
        fs.create_directory_verified("/BIG/NESTED").unwrap();
        // Several CHILD_BATCH multiples of files, so the paged copy and the
        // re-list-from-front delete both take multiple passes.
        for index in 0..100 {
            fs.create_empty(&alloc::format!("/BIG/F{index:03}.TXT"))
                .unwrap();
        }
        fs.save_verified("/BIG/NESTED/DATA.TXT", b"nested data")
            .unwrap();
        fs.move_entry_verified("/BIG", "/MOVED").unwrap();
        assert!(!fs.entry_exists("/BIG").unwrap());
        assert_eq!(
            fs.read_file("/MOVED/NESTED/DATA.TXT").unwrap(),
            b"nested data"
        );
        assert!(fs.entry_exists("/MOVED/F099.TXT").unwrap());
        fs.delete_verified("/MOVED", true).unwrap();
        assert!(!fs.entry_exists("/MOVED").unwrap());
    }

    #[test]
    fn too_deep_tree_is_refused_before_mutation() {
        let (_ram, fs) = formatted_fs(131_072);
        let mut path = String::new();
        for _ in 0..=MAX_TREE_DEPTH {
            path.push_str("/D");
            fs.create_directory_verified(&path).unwrap();
        }
        fs.save_verified("/D/CANARY.TXT", b"survives").unwrap();
        // The pre-scan meets the depth cap before either operation mutates
        // anything, so the shallow canary file must survive both refusals.
        assert!(matches!(
            fs.delete_verified("/D", true),
            Err(FsOpError::TooDeep)
        ));
        assert!(matches!(
            fs.move_entry_verified("/D", "/MOVED"),
            Err(FsOpError::TooDeep)
        ));
        assert_eq!(fs.read_file("/D/CANARY.TXT").unwrap(), b"survives");
        assert!(!fs.entry_exists("/MOVED").unwrap());
    }

    #[test]
    fn fat32_delete_reclaims_all_but_one_cluster() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let fat_lba = geometry.partition_start + 32;
        // Non-zero entries in the first FAT = allocated clusters (plus the
        // two reserved head entries).
        let count_used = |ram: &RamBlocks| {
            let blocks = ram.blocks.borrow();
            let mut used = 0usize;
            for sector in 0..geometry.fat_sectors {
                for entry in blocks[(fat_lba + sector) as usize].contents.chunks_exact(4) {
                    if u32::from_le_bytes(entry.try_into().unwrap()) != 0 {
                        used += 1;
                    }
                }
            }
            used
        };
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        let baseline = count_used(&ram);
        let contents = vec![0x5au8; 8 * 1024];
        let file_clusters = (contents.len() as u32)
            .div_ceil(u32::from(geometry.sectors_per_cluster) * 512)
            as usize;
        assert!(file_clusters > 1);
        fs.save_verified("/BIG.TXT", &contents).unwrap();
        assert_eq!(count_used(&ram), baseline + file_clusters);
        fs.delete_verified("/BIG.TXT", false).unwrap();
        // The manager's truncate keeps the anchor cluster; everything else
        // must come back on a FAT32 mount.
        assert_eq!(count_used(&ram), baseline + 1);

        // Gating: the reclaim is disabled when the volume was not probed as
        // FAT32, and embedded-sdmmc's delete never touches the FAT, so the
        // whole chain stays allocated.
        drop(fs);
        let fs = CardFs::mount(
            ram.clone(),
            MediaLayout {
                fat32: false,
                ..layout
            },
        )
        .unwrap();
        fs.save_verified("/LEAK.TXT", &contents).unwrap();
        let with_leak = count_used(&ram);
        fs.delete_verified("/LEAK.TXT", false).unwrap();
        assert_eq!(count_used(&ram), with_leak);
    }

    #[test]
    fn free_space_tracks_saves_and_deletes() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        let cluster_bytes = u64::from(geometry.sectors_per_cluster) * 512;

        // Freshly formatted: every cluster is free except the root
        // directory's, exactly as the formatter recorded in FSInfo.
        let initial = fs.free_space_bytes().unwrap();
        assert_eq!(
            initial,
            u64::from(geometry.cluster_count - 1) * cluster_bytes
        );

        let contents = vec![0x5au8; 8 * 1024];
        let file_clusters = u64::from(
            (contents.len() as u32).div_ceil(u32::from(geometry.sectors_per_cluster) * 512),
        );
        fs.save_verified("/BIG.TXT", &contents).unwrap();
        assert_eq!(
            fs.free_space_bytes().unwrap(),
            initial - file_clusters * cluster_bytes
        );

        // The reclaim frees all but the anchor cluster, and that must show
        // up here even though the manager only writes its running count to
        // the FSInfo sector on a volume cycle. The second missing cluster
        // is the manager's truncate accounting drift (see the
        // free_space_bytes docs): the chain's final cluster is freed in the
        // FAT without being counted.
        fs.delete_verified("/BIG.TXT", false).unwrap();
        assert_eq!(fs.free_space_bytes().unwrap(), initial - 2 * cluster_bytes);

        // The measurement cycled the volume; prove the filesystem still
        // works through the reopened handle.
        fs.save_verified("/AGAIN.TXT", b"still mounted").unwrap();
        assert_eq!(fs.read_file("/AGAIN.TXT").unwrap(), b"still mounted");
    }

    #[test]
    fn free_space_survives_transient_read_misses() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        // Transient flakes are the device layer's job now, so this test
        // mounts through RetryBlocks to mirror the firmware's controller
        // device.
        let fs = CardFs::mount(RetryBlocks(ram.clone()), layout).unwrap();
        let expected =
            u64::from(geometry.cluster_count - 1) * u64::from(geometry.sectors_per_cluster) * 512;

        // One transient miss per sector: real cards flake on single reads,
        // which the device layer retries.
        ram.fail_next_reads.set(1);
        assert_eq!(fs.free_space_bytes(), Some(expected));

        // A card that stops responding entirely reports unknown instead of
        // a stale or invented figure. The measurement was cached above, so
        // mark it stale the way any mutation would. The failed flush closed
        // the volume and the reopen failed too: the handle is now dead.
        fs.mark_free_space_stale();
        ram.fail_next_reads.set(u32::MAX);
        assert_eq!(fs.free_space_bytes(), None);
        // Still dead: each query retries only the reopen, and the card is
        // still not answering (3 device attempts = one logical read through
        // RetryBlocks).
        ram.fail_next_reads.set(3);
        assert_eq!(fs.free_space_bytes(), None);
        // Card recovered: the very next query repairs the handle and
        // re-measures -- browsing self-heals without waiting for a
        // mutation or a reseat. (The FlushFailed wait-for-a-mutation latch
        // only arises when the close itself is refused via
        // VolumeStillInUse, which no host fixture can produce since every
        // operation closes its handles.)
        ram.fail_next_reads.set(0);
        assert_eq!(fs.free_space_bytes(), Some(expected));
        // The repaired handle really works.
        fs.save_verified("/HEAL.TXT", b"alive").unwrap();
        assert_eq!(fs.read_file("/HEAL.TXT").unwrap(), b"alive");
    }

    #[test]
    fn volume_dead_heal_survives_a_flake_after_the_reopen() {
        // Once the VolumeDead reopen has succeeded the handle is repaired;
        // a single read flake anywhere after it (the measurement, or
        // whatever comes next) must never re-kill the fresh handle. The
        // old fall-through re-flush cycled the volume a second time and
        // handed exactly such a flake the chance to re-latch VolumeDead.
        // Plain RamBlocks: a single injected flake must reach the library.
        let dead_fs = || {
            let (ram, fs) = formatted_fs(131_072);
            fs.free_space_bytes().unwrap();
            fs.mark_free_space_stale();
            ram.fail_next_reads.set(u32::MAX);
            assert_eq!(
                fs.free_space_bytes(),
                None,
                "the flush must kill the handle"
            );
            ram.fail_next_reads.set(0);
            (ram, fs)
        };
        // Reads a bare successful reopen consumes, measured on a twin.
        let (ram, fs) = dead_fs();
        let before = ram.reads_seen.get();
        fs.reopen_volume().unwrap();
        let reopen_reads = ram.reads_seen.get() - before;

        for offset in 0..8u32 {
            let (ram, fs) = dead_fs();
            ram.fail_one_read_after.set(Some(reopen_reads + offset));
            // The reopen itself stays clean; the flake lands after it. The
            // figure may come back None (the flake hit the measurement) --
            // only the handle's health matters.
            let _ = fs.free_space_bytes();
            ram.fail_one_read_after.set(None);
            fs.save_verified("/HEAL.TXT", b"alive")
                .unwrap_or_else(|error| {
                    panic!("offset {offset}: handle still dead after heal: {error:?}")
                });
        }
    }

    #[test]
    fn unknown_fsinfo_free_count_reports_none() {
        let sectors = 131_072u32;
        let ram = RamBlocks::new(sectors);
        let geometry = format_fat32(&mut SdStream::new(ram.clone()).unwrap(), sectors, 0).unwrap();
        // A card formatted elsewhere may carry no free-cluster hint; the
        // manager then never maintains one either, so the figure must stay
        // unknown instead of surfacing 0xFFFF_FFFF clusters as bytes.
        ram.blocks.borrow_mut()[(geometry.partition_start + 1) as usize].contents[488..492]
            .copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), sectors).unwrap();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        assert_eq!(fs.free_space_bytes(), None);
        fs.save_verified("/A.TXT", b"data").unwrap();
        assert_eq!(fs.free_space_bytes(), None);
    }

    /// The FAT16 fixture's geometry, shared with the tests that assert
    /// against specific sectors so there is one source of truth.
    const FAT16_START: u32 = 1;
    const FAT16_RESERVED: u32 = 1;
    const FAT16_ROOT_SECTORS: u32 = 32;
    const FAT16_CLUSTERS: u32 = 4_085;

    /// A minimal mountable FAT16 volume: MBR slot 0, one-sector reserved
    /// area, two FATs, a 512-entry root directory, and just enough clusters
    /// to clear the FAT12 boundary. `fat_sectors` sizes each FAT copy (16
    /// holds every entry; less under-provisions the FAT), and `mark_tail`
    /// pre-marks the phantom tail the way a careful formatter -- or this
    /// crate's mount-time repair -- would, keeping the fixture's mounts
    /// write-free.
    fn fat16_media_custom(fat_sectors: u32, mark_tail: bool) -> (RamBlocks, MediaLayout) {
        let (ram, media_sectors) = fat16_media_image(fat_sectors, mark_tail);
        let layout = probe_fat(&mut SdStream::new(ram.clone()).unwrap(), media_sectors).unwrap();
        assert!(!layout.fat32);
        (ram, layout)
    }

    /// Build the raw FAT16 image without probing it, so tests of volumes
    /// the probe must refuse can share the fixture. Returns the media and
    /// its sector count.
    fn fat16_media_image(fat_sectors: u32, mark_tail: bool) -> (RamBlocks, u32) {
        let total = FAT16_RESERVED + 2 * fat_sectors + FAT16_ROOT_SECTORS + FAT16_CLUSTERS;
        let ram = RamBlocks::new(FAT16_START + total);
        {
            let mut blocks = ram.blocks.borrow_mut();
            let mbr = &mut blocks[0].contents;
            mbr[446 + 4] = 0x06;
            put_u32(&mut mbr[446 + 8..446 + 12], FAT16_START);
            put_u32(&mut mbr[446 + 12..446 + 16], total);
            mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
            let bpb = &mut blocks[FAT16_START as usize].contents;
            put_u16(&mut bpb[11..13], 512);
            bpb[13] = 1;
            put_u16(&mut bpb[14..16], FAT16_RESERVED as u16);
            bpb[16] = 2;
            put_u16(&mut bpb[17..19], 512);
            bpb[21] = 0xf8;
            put_u16(&mut bpb[22..24], fat_sectors as u16);
            put_u32(&mut bpb[32..36], total);
            put_u32(&mut bpb[39..43], 0x1616_1616);
            bpb[510..512].copy_from_slice(&[0x55, 0xaa]);
            for copy in 0..2u32 {
                blocks[(FAT16_START + FAT16_RESERVED + copy * fat_sectors) as usize].contents[0..4]
                    .copy_from_slice(&[0xf8, 0xff, 0xff, 0xff]);
            }
            // The FAT's last sector has spare entries beyond the volume's
            // real clusters, which the manager's free-cluster search would
            // hand out as allocatable clusters mapping past the end of the
            // media (mount repairs an unmarked tail for exactly that
            // reason).
            if mark_tail {
                let first_phantom = (fat_entry_bytes(FAT16_CLUSTERS, false) % 512) as usize;
                for copy in 0..2u32 {
                    let last_fat =
                        (FAT16_START + FAT16_RESERVED + (copy + 1) * fat_sectors - 1) as usize;
                    for entry in blocks[last_fat].contents[first_phantom..].chunks_exact_mut(2) {
                        entry.copy_from_slice(&[0xf7, 0xff]);
                    }
                }
            }
        }
        (ram, FAT16_START + total)
    }

    fn fat16_media() -> (RamBlocks, MediaLayout) {
        fat16_media_custom(16, true)
    }

    #[test]
    fn fat16_free_space_is_counted_from_the_fat() {
        let (ram, layout) = fat16_media();
        let fs = CardFs::mount(ram.clone(), layout).unwrap();
        let cluster_bytes = 512u64; // one sector per cluster
        assert_eq!(fs.free_space_bytes(), Some(4_085 * cluster_bytes));

        let contents = [0x16u8; 1536]; // three clusters
        fs.save_verified("/DATA.TXT", &contents).unwrap();
        assert_eq!(fs.free_space_bytes(), Some((4_085 - 3) * cluster_bytes));

        // Without the FAT32-only reclaim a deleted file's chain stays
        // allocated, and counting the FAT reports that honestly.
        fs.delete_verified("/DATA.TXT", false).unwrap();
        assert_eq!(fs.free_space_bytes(), Some((4_085 - 3) * cluster_bytes));
    }
}
