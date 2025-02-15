use std::collections::HashMap;

use nom::multi::fold_many_m_n;
use nom::{
    bytes::complete::take,
    combinator::verify,
    error::{Error as NomError, ErrorKind},
    number::complete::{le_u16, le_u32},
    sequence::tuple,
    IResult,
};

const OLECF_SIGNATURE: &[u8] =
    &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const SECTOR_SHIFT: u16 = 9;
const MINI_SECTOR_SHIFT: u16 = 6;
const DIRECTORY_ENTRY_SIZE: u64 = 128;

// Directory Entry Types
const STORAGE_TYPE: u8 = 1;
const STREAM_TYPE: u8 = 2;
const ROOT_STORAGE_TYPE: u8 = 5;

// Special sectors
const ENDOFCHAIN: u32 = 0xFFFFFFFE;
const MAX_REGULAR_SECTOR: u32 = 0xFFFFFFFA;

pub struct OLECFParser<'a> {
    data: &'a [u8],
    sector_size: usize,
    mini_sector_size: usize,
    fat_sectors: Vec<u32>,
    directory_sectors: Vec<u32>,
    mini_fat_sectors: Vec<u32>,
    dir_entries: HashMap<String, DirectoryEntry>,
    mini_stream_start: u32,
    mini_stream_size: u64,
}

pub struct DirectoryEntry {
    pub name: String,
    pub size: u64,
    pub start_sector: u32,
    pub stream_type: u8,
}

impl<'a> OLECFParser<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, &'static str> {
        let mut parser = OLECFParser {
            data,
            sector_size: 1 << SECTOR_SHIFT,
            mini_sector_size: 1 << MINI_SECTOR_SHIFT,
            fat_sectors: Vec::new(),
            directory_sectors: Vec::new(),
            mini_fat_sectors: Vec::new(),
            dir_entries: HashMap::new(),
            mini_stream_start: 0,
            mini_stream_size: 0,
        };

        match parser.parse(data) {
            Ok((_rest, ())) => Ok(parser),
            Err(_) => Err("Failed to parse OLECF data"),
        }
    }

    fn parse(&mut self, input: &'a [u8]) -> IResult<&'a [u8], ()> {
        let (input, ()) = self.parse_header(input)?;
        self.parse_directory(input)
    }

    /// Parses the Compound File Header.
    ///
    /// [MS-CFB] Section 2.2
    fn parse_header(&mut self, input: &'a [u8]) -> IResult<&'a [u8], ()> {
        let (
            input,
            (
                _signature,
                _clsid,
                _minor_version,
                _major_version,
                _byte_order,
                _sector_shift,
                _mini_sector_shift,
                _reserved,
                _num_dir_sectors,
                num_fat_sectors,
                first_dir_sector,
                _transaction_sig_num,
                _mini_stream_cutoff_size,
                first_mini_fat,
                mini_fat_count,
                _first_difat_sector,
                _difat_count,
            ),
        ) = tuple((
            verify(take(8_usize), |sig: &[u8]| sig == OLECF_SIGNATURE),
            take(16usize), // CLSID,
            le_u16,        // minor_version
            le_u16,        // major_version
            verify(le_u16, |byte_order| *byte_order == 0xFFFE),
            le_u16,       // sector_shift
            le_u16,       // mini_sector_shift
            take(6usize), // reserved
            le_u32,       // num_dir_sectors
            le_u32,       // num_fat_sectors
            le_u32,       // first_dir_sector
            le_u32,       // transaction_sig_num
            le_u32,       // mini_stream_cutoff_size
            le_u32,       // first_mini_fat
            le_u32,       // mini_fat_count
            le_u32,       // _first_difat_sector
            le_u32,       // _difat_count
        ))(input)?;

        // Parse the first 109 DIFAT entries, which are contained in the
        // header sector.
        let (input, _) = fold_many_m_n(
            0,
            109,
            le_u32,
            || {},
            |_, sector| {
                if sector < MAX_REGULAR_SECTOR {
                    self.fat_sectors.push(sector);
                }
            },
        )(input)?;

        // (C) Directory chain
        if first_dir_sector < MAX_REGULAR_SECTOR {
            self.directory_sectors = self.follow_chain(first_dir_sector);
        } else {
            return Err(nom::Err::Error(NomError::new(
                input,
                ErrorKind::Verify,
            )));
        }

        // (D) MiniFAT chain
        if mini_fat_count > 0 && first_mini_fat < MAX_REGULAR_SECTOR {
            self.mini_fat_sectors = self.follow_chain(first_mini_fat);
        }

        // (E) If no FAT sectors but num_fat_sectors != 0 => error
        if self.fat_sectors.is_empty() && num_fat_sectors > 0 {
            return Err(nom::Err::Error(NomError::new(
                input,
                ErrorKind::Verify,
            )));
        }

        Ok((input, ()))
    }

    fn parse_directory(&mut self, _input: &'a [u8]) -> IResult<&'a [u8], ()> {
        if self.directory_sectors.is_empty() {
            return Err(nom::Err::Error(NomError::new(
                _input,
                ErrorKind::Verify,
            )));
        }

        for &sector in &self.directory_sectors {
            let mut entry_offset = 0;

            while entry_offset + DIRECTORY_ENTRY_SIZE as usize
                <= self.sector_size
            {
                let abs_offset = self.sector_to_offset(sector) + entry_offset;
                if abs_offset + DIRECTORY_ENTRY_SIZE as usize > self.data.len()
                {
                    break;
                }
                if let Ok(entry) = self.read_directory_entry(abs_offset) {
                    if entry.stream_type == ROOT_STORAGE_TYPE {
                        self.mini_stream_start = entry.start_sector;
                        self.mini_stream_size = entry.size;
                    }
                    if entry.stream_type == STORAGE_TYPE
                        || entry.stream_type == STREAM_TYPE
                        || entry.stream_type == ROOT_STORAGE_TYPE
                    {
                        self.dir_entries.insert(entry.name.clone(), entry);
                    }
                }
                entry_offset += DIRECTORY_ENTRY_SIZE as usize;
            }
        }

        Ok((_input, ()))
    }

    pub fn is_valid_header(&self) -> bool {
        self.data.len() >= OLECF_SIGNATURE.len()
            && &self.data[..OLECF_SIGNATURE.len()] == OLECF_SIGNATURE
    }

    pub fn get_stream_names(&self) -> Result<Vec<String>, &'static str> {
        if self.dir_entries.is_empty() {
            return Err("No streams found");
        }
        Ok(self.dir_entries.keys().cloned().collect())
    }

    pub fn get_stream_size(
        &self,
        stream_name: &str,
    ) -> Result<u64, &'static str> {
        self.dir_entries
            .get(stream_name)
            .map(|e| e.size)
            .ok_or("Stream not found")
    }

    pub fn get_streams(
        &self,
    ) -> impl Iterator<Item = (&str, &DirectoryEntry)> {
        self.dir_entries.iter().map(|(name, entry)| (name.as_str(), entry))
    }

    pub fn get_stream_data(
        &self,
        stream_name: &str,
    ) -> Result<Vec<u8>, &'static str> {
        let entry =
            self.dir_entries.get(stream_name).ok_or("Stream not found")?;

        if entry.size < 4096 && entry.stream_type != ROOT_STORAGE_TYPE {
            self.get_mini_stream_data(entry.start_sector, entry.size)
        } else {
            self.get_regular_stream_data(entry.start_sector, entry.size)
        }
    }

    fn sector_to_offset(&self, sector: u32) -> usize {
        // The first sector begins at offset 512
        512 + (sector as usize * self.sector_size)
    }

    fn read_sector(&self, sector: u32) -> Result<&[u8], &'static str> {
        let offset = self.sector_to_offset(sector);
        if offset + self.sector_size > self.data.len() {
            return Err("Sector read out of bounds");
        }
        Ok(&self.data[offset..offset + self.sector_size])
    }

    fn get_fat_entry(&self, sector: u32) -> Result<u32, &'static str> {
        let entry_index = sector as usize;
        let entries_per_sector = self.sector_size / 4;
        let fat_sector_index = entry_index / entries_per_sector;
        if fat_sector_index >= self.fat_sectors.len() {
            return Err("FAT entry sector index out of range");
        }
        let fat_sector = self.fat_sectors[fat_sector_index];
        let fat = self.read_sector(fat_sector)?;
        let fat_entry_offset = (entry_index % entries_per_sector) * 4;
        parse_u32_at(fat, fat_entry_offset)
    }

    fn follow_chain(&self, start_sector: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut current = start_sector;

        loop {
            // Ensure that the current sector is a valid one.
            if current > MAX_REGULAR_SECTOR {
                break;
            }

            // Prevent cycles by keeping track of visited sectors
            if chain.contains(&current) {
                // We've seen this sector before - it's a cycle
                break;
            }

            chain.push(current);

            // Now current is the next entry in the chain.
            current = match self.get_fat_entry(current) {
                Err(_) => break,
                Ok(n) if n == ENDOFCHAIN => break,
                Ok(n) => n,
            };
        }

        chain
    }

    fn read_directory_entry(
        &self,
        offset: usize,
    ) -> Result<DirectoryEntry, &'static str> {
        if offset + 128 > self.data.len() {
            return Err("Incomplete directory entry");
        }

        let name_len = parse_u16_at(self.data, offset + 64)? as usize;
        if !(2..=64).contains(&name_len) {
            return Err("Invalid name length");
        }

        let name_bytes = &self.data[offset..offset + name_len];
        let filtered: Vec<u8> =
            name_bytes.iter().copied().filter(|&b| b != 0).collect();
        let name = String::from_utf8_lossy(&filtered).to_string();

        let stream_type = self.data[offset + 66];
        let start_sector = parse_u32_at(self.data, offset + 116)?;
        let size_32 = parse_u32_at(self.data, offset + 120)?;
        let size = size_32 as u64;

        Ok(DirectoryEntry { name, size, start_sector, stream_type })
    }

    fn get_regular_stream_data(
        &self,
        start_sector: u32,
        size: u64,
    ) -> Result<Vec<u8>, &'static str> {
        let mut data = Vec::with_capacity(size as usize);
        let mut current_sector = start_sector;
        let mut total_read = 0;

        while current_sector < MAX_REGULAR_SECTOR && total_read < size as usize
        {
            let sector_data = self.read_sector(current_sector)?;
            let bytes_to_read =
                std::cmp::min(self.sector_size, size as usize - total_read);

            data.extend_from_slice(&sector_data[..bytes_to_read]);
            total_read += bytes_to_read;

            if total_read < size as usize {
                let next = self.get_fat_entry(current_sector)?;
                if next == ENDOFCHAIN || next >= MAX_REGULAR_SECTOR {
                    break;
                }
                current_sector = next;
            }
        }

        if data.len() != size as usize {
            return Err("Incomplete stream data");
        }

        Ok(data)
    }

    fn get_root_mini_stream_data(&self) -> Result<Vec<u8>, &'static str> {
        self.get_regular_stream_data(
            self.mini_stream_start,
            self.mini_stream_size,
        )
    }

    fn get_minifat_entry(
        &self,
        mini_sector: u32,
    ) -> Result<u32, &'static str> {
        if self.mini_fat_sectors.is_empty() {
            return Ok(ENDOFCHAIN);
        }

        let entry_index = mini_sector as usize;
        let entries_per_sector = self.sector_size / 4;
        let fat_sector_index = entry_index / entries_per_sector;
        if fat_sector_index >= self.mini_fat_sectors.len() {
            return Ok(ENDOFCHAIN);
        }
        let sector = self.mini_fat_sectors[fat_sector_index];
        let fat = self.read_sector(sector)?;
        let offset = (entry_index % entries_per_sector) * 4;
        parse_u32_at(fat, offset)
    }

    fn get_mini_stream_data(
        &self,
        start_mini_sector: u32,
        size: u64,
    ) -> Result<Vec<u8>, &'static str> {
        if self.mini_stream_size == 0 {
            return Err("No mini stream present");
        }

        let mini_stream_data = self.get_root_mini_stream_data()?;
        let mini_data_len = mini_stream_data.len();

        let mut data = Vec::with_capacity(size as usize);
        let mut current = start_mini_sector;

        while current < MAX_REGULAR_SECTOR && data.len() < size as usize {
            let mini_offset = current as usize * self.mini_sector_size;
            if mini_offset >= mini_data_len {
                return Err("Mini stream offset out of range");
            }

            let bytes_to_read = std::cmp::min(
                self.mini_sector_size,
                size as usize - data.len(),
            );
            if mini_offset + bytes_to_read > mini_data_len {
                return Err("Mini stream extends beyond available data");
            }

            data.extend_from_slice(
                &mini_stream_data[mini_offset..mini_offset + bytes_to_read],
            );

            if data.len() < size as usize {
                let next = self.get_minifat_entry(current)?;
                if next == ENDOFCHAIN || next >= MAX_REGULAR_SECTOR {
                    break;
                }
                current = next;
            }
        }

        if data.len() != size as usize {
            return Err("Incomplete mini stream data");
        }

        Ok(data)
    }
}

fn parse_u16_at(data: &[u8], offset: usize) -> Result<u16, &'static str> {
    if offset + 2 > data.len() {
        return Err("Buffer too small for u16");
    }
    let slice = &data[offset..offset + 2];
    match le_u16::<&[u8], NomError<&[u8]>>(slice) {
        Ok((_, val)) => Ok(val),
        Err(_) => Err("Failed to parse u16"),
    }
}

fn parse_u32_at(data: &[u8], offset: usize) -> Result<u32, &'static str> {
    if offset + 4 > data.len() {
        return Err("Buffer too small for u32");
    }
    let slice = &data[offset..offset + 4];
    match le_u32::<&[u8], NomError<&[u8]>>(slice) {
        Ok((_, val)) => Ok(val),
        Err(_) => Err("Failed to parse u32"),
    }
}
