use crate::fits_table::ObservationMetadata;
use chrono::Utc;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    mem::size_of,
    path::Path,
    time::Instant,
};

pub const BINARY_MAGIC: [u8; 8] = *b"HIOBIN01";
pub const BINARY_VERSION: u32 = 1;
pub const BINARY_HEADER_SIZE: usize = 256;
pub const BINARY_ROW_HEADER_SIZE: usize = 16;

const ENDIAN_MARKER: u32 = 0x0102_0304;
const FORMAT_FLAGS: u32 = 0x0000_0003;
const VALUE_TYPE_FLOAT32_POWER: u32 = 1;

const ROW_COUNT_OFFSET: u64 = 128;
const END_UNIX_NS_OFFSET: u64 = 136;
const ELAPSED_SEC_OFFSET: u64 = 144;

pub struct BinaryObservationWriter {
    writer: BufWriter<File>,
    metadata: ObservationMetadata,
    observation_start_unix_ns: i64,
    elapsed_clock: Instant,
    elapsed_offset_sec: f64,
    row_count: u64,
}

impl BinaryObservationWriter {
    pub fn open(path: &Path, metadata: ObservationMetadata, append: bool) -> io::Result<Self> {
        if append && path.metadata().is_ok_and(|entry| entry.len() > 0) {
            Self::append(path, metadata)
        } else {
            Self::create(path, metadata)
        }
    }

    fn create(path: &Path, metadata: ObservationMetadata) -> io::Result<Self> {
        let observation_start_unix_ns = unix_nanoseconds_now()?;
        let row_size = checked_row_size(metadata.channel_count)?;
        let header = encode_header(
            &metadata,
            observation_start_unix_ns,
            observation_start_unix_ns,
            row_size,
            0,
            0.0,
        )?;
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(&header)?;

        Ok(Self {
            writer,
            metadata,
            observation_start_unix_ns,
            elapsed_clock: Instant::now(),
            elapsed_offset_sec: 0.0,
            row_count: 0,
        })
    }

    fn append(path: &Path, metadata: ObservationMetadata) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut header = [0_u8; BINARY_HEADER_SIZE];
        file.read_exact(&mut header)?;
        let stored = decode_header(&header)?;
        validate_append_metadata(&stored, &metadata)?;

        let file_size = file.metadata()?.len();
        let data_size = file_size
            .checked_sub(BINARY_HEADER_SIZE as u64)
            .ok_or_else(|| invalid_data("binary file is shorter than its header"))?;
        if data_size % stored.row_size as u64 != 0 {
            return Err(invalid_data(
                "binary file ends with an incomplete spectrum record",
            ));
        }
        let row_count = data_size / stored.row_size as u64;
        let last_elapsed_sec = if row_count == 0 {
            0.0
        } else {
            let last_row = BINARY_HEADER_SIZE as u64 + (row_count - 1) * stored.row_size as u64;
            file.seek(SeekFrom::Start(last_row))?;
            let mut value = [0_u8; 8];
            file.read_exact(&mut value)?;
            f64::from_le_bytes(value)
        };

        let wall_elapsed_sec = (unix_nanoseconds_now()? - stored.start_unix_ns) as f64 * 1e-9;
        let elapsed_offset_sec = wall_elapsed_sec.max(if row_count == 0 {
            0.0
        } else {
            last_elapsed_sec + metadata.time_resolution_sec
        });
        file.seek(SeekFrom::End(0))?;

        Ok(Self {
            writer: BufWriter::new(file),
            metadata,
            observation_start_unix_ns: stored.start_unix_ns,
            elapsed_clock: Instant::now(),
            elapsed_offset_sec,
            row_count,
        })
    }

    pub fn write_spectrum(&mut self, center_frequency_hz: f64, spectrum: &[f32]) -> io::Result<()> {
        if spectrum.len() != self.metadata.channel_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "expected {} binary channels, received {}",
                    self.metadata.channel_count,
                    spectrum.len()
                ),
            ));
        }

        let elapsed_sec = self.elapsed_offset_sec + self.elapsed_clock.elapsed().as_secs_f64();
        self.writer.write_all(&elapsed_sec.to_le_bytes())?;
        self.writer.write_all(&center_frequency_hz.to_le_bytes())?;
        for value in spectrum {
            self.writer.write_all(&value.to_le_bytes())?;
        }
        self.row_count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let elapsed_sec = self.elapsed_offset_sec + self.elapsed_clock.elapsed().as_secs_f64();
        let end_unix_ns = unix_nanoseconds_now()?;
        self.writer.flush()?;
        let file_end = self.writer.stream_position()?;

        self.writer.seek(SeekFrom::Start(ROW_COUNT_OFFSET))?;
        self.writer.write_all(&self.row_count.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(END_UNIX_NS_OFFSET))?;
        self.writer.write_all(&end_unix_ns.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(ELAPSED_SEC_OFFSET))?;
        self.writer.write_all(&elapsed_sec.to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(file_end))?;
        self.writer.flush()
    }

    pub fn observation_start_unix_ns(&self) -> i64 {
        self.observation_start_unix_ns
    }
}

struct DecodedHeader {
    start_unix_ns: i64,
    center_frequency_hz: f64,
    sample_rate_hz: f64,
    channel_count: usize,
    average_count: usize,
    pfb_taps: usize,
    time_resolution_sec: f64,
    lna_gain_db: f64,
    mix_gain_db: f64,
    vga_gain_db: f64,
    row_size: usize,
}

fn checked_row_size(channel_count: usize) -> io::Result<usize> {
    BINARY_ROW_HEADER_SIZE
        .checked_add(
            channel_count
                .checked_mul(size_of::<f32>())
                .ok_or_else(|| invalid_input("binary row size overflow"))?,
        )
        .ok_or_else(|| invalid_input("binary row size overflow"))
}

fn encode_header(
    metadata: &ObservationMetadata,
    start_unix_ns: i64,
    end_unix_ns: i64,
    row_size: usize,
    row_count: u64,
    elapsed_sec: f64,
) -> io::Result<[u8; BINARY_HEADER_SIZE]> {
    let channel_count = u32::try_from(metadata.channel_count)
        .map_err(|_| invalid_input("channel count does not fit in the binary header"))?;
    let average_count = u32::try_from(metadata.average_count)
        .map_err(|_| invalid_input("average count does not fit in the binary header"))?;
    let pfb_taps = u32::try_from(metadata.pfb_taps)
        .map_err(|_| invalid_input("PFB tap count does not fit in the binary header"))?;
    let row_size = u64::try_from(row_size)
        .map_err(|_| invalid_input("row size does not fit in the binary header"))?;
    let channel_width_hz = metadata.sample_rate_hz / metadata.channel_count as f64;
    let frequency_min_hz = metadata.center_frequency_hz - metadata.sample_rate_hz / 2.0;
    let frequency_max_hz = metadata.center_frequency_hz + metadata.sample_rate_hz / 2.0;

    let mut header = [0_u8; BINARY_HEADER_SIZE];
    header[0..8].copy_from_slice(&BINARY_MAGIC);
    put_u32(&mut header, 8, BINARY_VERSION);
    put_u32(&mut header, 12, BINARY_HEADER_SIZE as u32);
    put_u32(&mut header, 16, ENDIAN_MARKER);
    put_u32(&mut header, 20, FORMAT_FLAGS);
    put_i64(&mut header, 24, start_unix_ns);
    put_f64(&mut header, 32, metadata.center_frequency_hz);
    put_f64(&mut header, 40, metadata.sample_rate_hz);
    put_f64(&mut header, 48, metadata.sample_rate_hz);
    put_f64(&mut header, 56, channel_width_hz);
    put_f64(&mut header, 64, metadata.time_resolution_sec);
    put_u32(&mut header, 72, channel_count);
    put_u32(&mut header, 76, average_count);
    put_u32(&mut header, 80, pfb_taps);
    put_u32(&mut header, 84, VALUE_TYPE_FLOAT32_POWER);
    put_f64(&mut header, 88, metadata.lna_gain_db);
    put_f64(&mut header, 96, metadata.mix_gain_db);
    put_f64(&mut header, 104, metadata.vga_gain_db);
    put_u32(&mut header, 112, BINARY_ROW_HEADER_SIZE as u32);
    put_u32(&mut header, 116, size_of::<f32>() as u32);
    put_u64(&mut header, 120, row_size);
    put_u64(&mut header, 128, row_count);
    put_i64(&mut header, 136, end_unix_ns);
    put_f64(&mut header, 144, elapsed_sec);
    put_f64(&mut header, 152, frequency_min_hz);
    put_f64(&mut header, 160, frequency_max_hz);
    Ok(header)
}

fn decode_header(header: &[u8; BINARY_HEADER_SIZE]) -> io::Result<DecodedHeader> {
    if header[0..8] != BINARY_MAGIC {
        return Err(invalid_data(
            "existing .bin file uses the legacy raw format; choose a new file name",
        ));
    }
    if get_u32(header, 8) != BINARY_VERSION {
        return Err(invalid_data("unsupported HI Observer binary version"));
    }
    if get_u32(header, 12) as usize != BINARY_HEADER_SIZE
        || get_u32(header, 16) != ENDIAN_MARKER
        || get_u32(header, 84) != VALUE_TYPE_FLOAT32_POWER
        || get_u32(header, 112) as usize != BINARY_ROW_HEADER_SIZE
        || get_u32(header, 116) as usize != size_of::<f32>()
    {
        return Err(invalid_data("invalid HI Observer binary header"));
    }

    let channel_count = get_u32(header, 72) as usize;
    let row_size = usize::try_from(get_u64(header, 120))
        .map_err(|_| invalid_data("binary row size is too large"))?;
    if channel_count == 0 || checked_row_size(channel_count)? != row_size {
        return Err(invalid_data("binary header has an invalid row layout"));
    }

    Ok(DecodedHeader {
        start_unix_ns: get_i64(header, 24),
        center_frequency_hz: get_f64(header, 32),
        sample_rate_hz: get_f64(header, 40),
        channel_count,
        average_count: get_u32(header, 76) as usize,
        pfb_taps: get_u32(header, 80) as usize,
        time_resolution_sec: get_f64(header, 64),
        lna_gain_db: get_f64(header, 88),
        mix_gain_db: get_f64(header, 96),
        vga_gain_db: get_f64(header, 104),
        row_size,
    })
}

fn validate_append_metadata(
    stored: &DecodedHeader,
    current: &ObservationMetadata,
) -> io::Result<()> {
    let matches = stored.channel_count == current.channel_count
        && stored.average_count == current.average_count
        && stored.pfb_taps == current.pfb_taps
        && close_f64(stored.center_frequency_hz, current.center_frequency_hz)
        && close_f64(stored.sample_rate_hz, current.sample_rate_hz)
        && close_f64(stored.time_resolution_sec, current.time_resolution_sec)
        && close_f64(stored.lna_gain_db, current.lna_gain_db)
        && close_f64(stored.mix_gain_db, current.mix_gain_db)
        && close_f64(stored.vga_gain_db, current.vga_gain_db);
    if matches {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot append: observation settings do not match the existing binary header",
        ))
    }
}

fn close_f64(left: f64, right: f64) -> bool {
    (left - right).abs() <= left.abs().max(right.abs()).max(1.0) * 1e-12
}

fn unix_nanoseconds_now() -> io::Result<i64> {
    Utc::now()
        .timestamp_nanos_opt()
        .ok_or_else(|| io::Error::other("UTC time is outside the supported nanosecond range"))
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(buffer: &mut [u8], offset: usize, value: i64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_f64(buffer: &mut [u8], offset: usize, value: f64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap())
}

fn get_u64(buffer: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap())
}

fn get_i64(buffer: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap())
}

fn get_f64(buffer: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(channel_count: usize) -> ObservationMetadata {
        ObservationMetadata {
            center_frequency_hz: 1_420_405_751.0,
            sample_rate_hz: 6_000_000.0,
            channel_count,
            average_count: 128,
            pfb_taps: 4,
            time_resolution_sec: 0.01,
            lna_gain_db: 5.0,
            mix_gain_db: 6.0,
            vga_gain_db: 7.0,
        }
    }

    #[test]
    fn writes_versioned_header_and_timed_rows() {
        let path = std::env::temp_dir().join(format!(
            "hi_observer_binary_test_{}.bin",
            std::process::id()
        ));
        let values = [1.25_f32, 2.5_f32, 5.0_f32];
        let center_frequency_hz = 1_420_405_751.0;
        let mut writer =
            BinaryObservationWriter::open(&path, metadata(values.len()), false).unwrap();
        writer.write_spectrum(center_frequency_hz, &values).unwrap();
        writer.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..8], &BINARY_MAGIC);
        assert_eq!(get_u32(&bytes, 8), BINARY_VERSION);
        assert_eq!(get_u32(&bytes, 72), values.len() as u32);
        assert_eq!(get_u64(&bytes, 128), 1);
        assert_eq!(
            bytes.len(),
            BINARY_HEADER_SIZE + BINARY_ROW_HEADER_SIZE + values.len() * 4
        );

        let data_offset = BINARY_HEADER_SIZE;
        assert!(get_f64(&bytes, data_offset) >= 0.0);
        assert_eq!(get_f64(&bytes, data_offset + 8), center_frequency_hz);
        for (index, expected) in values.iter().enumerate() {
            let start = data_offset + BINARY_ROW_HEADER_SIZE + index * 4;
            let stored = f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
            assert_eq!(stored, *expected);
        }

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn appends_compatible_observation_rows() {
        let path = std::env::temp_dir().join(format!(
            "hi_observer_binary_append_test_{}.bin",
            std::process::id()
        ));
        let values = [1.0_f32, 2.0_f32];
        let mut writer =
            BinaryObservationWriter::open(&path, metadata(values.len()), false).unwrap();
        writer.write_spectrum(100.0, &values).unwrap();
        writer.finish().unwrap();

        let mut writer =
            BinaryObservationWriter::open(&path, metadata(values.len()), true).unwrap();
        writer.write_spectrum(200.0, &values).unwrap();
        writer.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let row_size = BINARY_ROW_HEADER_SIZE + values.len() * 4;
        assert_eq!(get_u64(&bytes, 128), 2);
        assert_eq!(bytes.len(), BINARY_HEADER_SIZE + row_size * 2);
        assert_eq!(get_f64(&bytes, BINARY_HEADER_SIZE + row_size + 8), 200.0);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn refuses_to_append_to_legacy_raw_binary() {
        let path = std::env::temp_dir().join(format!(
            "hi_observer_binary_legacy_test_{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, [0_u8; BINARY_HEADER_SIZE]).unwrap();

        let error = BinaryObservationWriter::open(&path, metadata(2), true)
            .err()
            .expect("legacy append should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        std::fs::remove_file(path).unwrap();
    }
}
