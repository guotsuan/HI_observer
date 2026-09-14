use chrono::{DateTime, Utc};
use std::{
    fs::File,
    io::{self, BufWriter, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

const FITS_BLOCK_SIZE: usize = 2880;
const FITS_CARD_SIZE: usize = 80;

#[derive(Clone, Debug)]
pub struct ObservationMetadata {
    pub center_frequency_hz: f64,
    pub sample_rate_hz: f64,
    pub channel_count: usize,
    pub average_count: usize,
    pub pfb_taps: usize,
    pub time_resolution_sec: f64,
    pub lna_gain_db: f64,
    pub mix_gain_db: f64,
    pub vga_gain_db: f64,
}

pub struct FitsTableWriter {
    writer: BufWriter<File>,
    metadata: ObservationMetadata,
    observation_start: DateTime<Utc>,
    elapsed_clock: Instant,
    row_count: u64,
    row_size: usize,
    naxis2_offset: u64,
    date_end_offset: u64,
    tstop_offset: u64,
    telapse_offset: u64,
}

impl FitsTableWriter {
    pub fn create(path: &Path, metadata: ObservationMetadata) -> io::Result<Self> {
        let observation_start = Utc::now();
        let mut writer = BufWriter::new(File::create(path)?);
        write_primary_header(&mut writer, observation_start)?;

        let row_size = 16_usize
            .checked_add(metadata.channel_count.checked_mul(4).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "FITS row size overflow")
            })?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "FITS row size overflow"))?;
        let offsets =
            write_binary_table_header(&mut writer, observation_start, &metadata, row_size)?;

        Ok(Self {
            writer,
            metadata,
            observation_start,
            elapsed_clock: Instant::now(),
            row_count: 0,
            row_size,
            naxis2_offset: offsets.naxis2,
            date_end_offset: offsets.date_end,
            tstop_offset: offsets.tstop,
            telapse_offset: offsets.telapse,
        })
    }

    pub fn write_spectrum(&mut self, center_frequency_hz: f64, spectrum: &[f32]) -> io::Result<()> {
        if spectrum.len() != self.metadata.channel_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "expected {} FITS channels, received {}",
                    self.metadata.channel_count,
                    spectrum.len()
                ),
            ));
        }

        let elapsed_sec = self.elapsed_clock.elapsed().as_secs_f64();
        self.writer.write_all(&elapsed_sec.to_be_bytes())?;
        self.writer.write_all(&center_frequency_hz.to_be_bytes())?;
        for value in spectrum {
            self.writer.write_all(&value.to_be_bytes())?;
        }
        self.row_count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let elapsed_sec = self.elapsed_clock.elapsed().as_secs_f64();
        let data_size = (self.row_count as usize)
            .checked_mul(self.row_size)
            .ok_or_else(|| io::Error::other("FITS data size overflow"))?;
        let padding = padding_for(data_size);
        if padding > 0 {
            self.writer.write_all(&vec![0_u8; padding])?;
        }
        self.writer.flush()?;
        let file_end = self.writer.stream_position()?;

        patch_card(
            &mut self.writer,
            self.naxis2_offset,
            integer_card("NAXIS2", self.row_count, "Number of spectra"),
        )?;
        patch_card(
            &mut self.writer,
            self.date_end_offset,
            string_card(
                "DATE-END",
                &fits_datetime(Utc::now()),
                "UTC end of observation",
            ),
        )?;
        patch_card(
            &mut self.writer,
            self.tstop_offset,
            float_card("TSTOP", elapsed_sec, "Seconds from DATE-OBS"),
        )?;
        patch_card(
            &mut self.writer,
            self.telapse_offset,
            float_card("TELAPSE", elapsed_sec, "Elapsed observation time (s)"),
        )?;

        self.writer.seek(SeekFrom::Start(file_end))?;
        self.writer.flush()
    }

    pub fn observation_start(&self) -> DateTime<Utc> {
        self.observation_start
    }
}

struct HeaderOffsets {
    naxis2: u64,
    date_end: u64,
    tstop: u64,
    telapse: u64,
}

fn write_primary_header(writer: &mut BufWriter<File>, created: DateTime<Utc>) -> io::Result<()> {
    let cards = vec![
        logical_card("SIMPLE", true, "Conforms to FITS standard"),
        integer_card("BITPIX", 8, "Character data"),
        integer_card("NAXIS", 0, "No primary data array"),
        logical_card("EXTEND", true, "Extensions may be present"),
        string_card("DATE", &fits_datetime(created), "UTC file creation time"),
        string_card("ORIGIN", "HI Observer", "Data acquisition software"),
        string_card("CREATOR", "channelize", "HI Observer channelizer"),
        end_card(),
    ];
    write_header(writer, &cards)
}

fn write_binary_table_header(
    writer: &mut BufWriter<File>,
    observation_start: DateTime<Utc>,
    metadata: &ObservationMetadata,
    row_size: usize,
) -> io::Result<HeaderOffsets> {
    let start_offset = writer.stream_position()?;
    let channel_width_hz = metadata.sample_rate_hz / metadata.channel_count as f64;
    let frequency_min_hz = metadata.center_frequency_hz - metadata.sample_rate_hz / 2.0;
    let frequency_max_hz = metadata.center_frequency_hz + metadata.sample_rate_hz / 2.0;
    let mjd_reference = unix_seconds(observation_start) / 86_400.0 + 40_587.0;

    let mut cards = vec![
        string_card("XTENSION", "BINTABLE", "Binary table extension"),
        integer_card("BITPIX", 8, "Character data"),
        integer_card("NAXIS", 2, "Table is a matrix"),
        integer_card("NAXIS1", row_size as u64, "Bytes per table row"),
        integer_card("NAXIS2", 0, "Number of spectra"),
        integer_card("PCOUNT", 0, "No heap data"),
        integer_card("GCOUNT", 1, "One data group"),
        integer_card("TFIELDS", 3, "Number of table fields"),
        string_card("EXTNAME", "SPECTRA", "Spectrum time series"),
        string_card("TTYPE1", "TIME", "Elapsed time from DATE-OBS"),
        string_card("TFORM1", "1D", "64-bit floating point"),
        string_card("TUNIT1", "s", "Seconds"),
        string_card("TTYPE2", "FREQUENCY", "Center frequency"),
        string_card("TFORM2", "1D", "64-bit floating point"),
        string_card("TUNIT2", "Hz", "Frequency unit"),
        string_card("TTYPE3", "SPECTRUM", "Averaged linear power"),
        string_card(
            "TFORM3",
            &format!("{}E", metadata.channel_count),
            "32-bit floating-point vector",
        ),
        string_card("TUNIT3", "arbitrary", "Uncalibrated linear power"),
        string_card(
            "TDIM3",
            &format!("({})", metadata.channel_count),
            "Spectrum vector dimensions",
        ),
        string_card(
            "DATE-OBS",
            &fits_datetime(observation_start),
            "UTC start of observation",
        ),
        string_card(
            "DATE-END",
            &fits_datetime(observation_start),
            "UTC end of observation",
        ),
        string_card("TIMESYS", "UTC", "Time scale"),
        string_card("TIMEUNIT", "s", "Time unit"),
        string_card("TREFPOS", "TOPOCENTER", "Time reference position"),
        float_card("MJDREF", mjd_reference, "MJD at TIME=0"),
        float_card("TSTART", 0.0, "Seconds from DATE-OBS"),
        float_card("TSTOP", 0.0, "Seconds from DATE-OBS"),
        float_card("TELAPSE", 0.0, "Elapsed observation time (s)"),
        float_card(
            "TIMEDEL",
            metadata.time_resolution_sec,
            "Nominal spectrum interval (s)",
        ),
        float_card(
            "OBSFREQ",
            metadata.center_frequency_hz,
            "Initial center frequency (Hz)",
        ),
        float_card(
            "SAMPRATE",
            metadata.sample_rate_hz,
            "ADC sample rate (sample/s)",
        ),
        float_card("BANDWID", metadata.sample_rate_hz, "Bandwidth (Hz)"),
        float_card("FREQMIN", frequency_min_hz, "Lower band edge (Hz)"),
        float_card("FREQMAX", frequency_max_hz, "Upper band edge (Hz)"),
        float_card("CHAN_BW", channel_width_hz, "Channel width (Hz)"),
        integer_card(
            "NCHANS",
            metadata.channel_count as u64,
            "Number of frequency channels",
        ),
        integer_card(
            "NAVG",
            metadata.average_count as u64,
            "Spectra averaged per row",
        ),
        integer_card("PFBTAPS", metadata.pfb_taps as u64, "PFB taps per channel"),
        float_card("LNAGAIN", metadata.lna_gain_db, "LNA gain setting (dB)"),
        float_card("MIXGAIN", metadata.mix_gain_db, "Mixer gain setting (dB)"),
        float_card("VGAGAIN", metadata.vga_gain_db, "VGA gain setting (dB)"),
        string_card("DATATYPE", "POWER", "Stored spectrum quantity"),
        string_card("SPECSYS", "TOPOCENT", "Topocentric spectral frame"),
        end_card(),
    ];

    let index_of = |keyword: &str| -> io::Result<u64> {
        cards
            .iter()
            .position(|card| card.starts_with(keyword.as_bytes()))
            .map(|index| start_offset + (index * FITS_CARD_SIZE) as u64)
            .ok_or_else(|| io::Error::other(format!("missing FITS card {keyword}")))
    };
    let offsets = HeaderOffsets {
        naxis2: index_of("NAXIS2")?,
        date_end: index_of("DATE-END")?,
        tstop: index_of("TSTOP")?,
        telapse: index_of("TELAPSE")?,
    };

    write_header(writer, &cards)?;
    cards.clear();
    Ok(offsets)
}

fn write_header(writer: &mut BufWriter<File>, cards: &[[u8; FITS_CARD_SIZE]]) -> io::Result<()> {
    for card in cards {
        writer.write_all(card)?;
    }
    let padding = padding_for(cards.len() * FITS_CARD_SIZE);
    if padding > 0 {
        writer.write_all(&vec![b' '; padding])?;
    }
    Ok(())
}

fn patch_card(
    writer: &mut BufWriter<File>,
    offset: u64,
    card: [u8; FITS_CARD_SIZE],
) -> io::Result<()> {
    writer.seek(SeekFrom::Start(offset))?;
    writer.write_all(&card)
}

fn padding_for(byte_count: usize) -> usize {
    (FITS_BLOCK_SIZE - byte_count % FITS_BLOCK_SIZE) % FITS_BLOCK_SIZE
}

fn fits_datetime(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

fn unix_seconds(value: DateTime<Utc>) -> f64 {
    value.timestamp() as f64 + value.timestamp_subsec_nanos() as f64 * 1e-9
}

fn logical_card(keyword: &str, value: bool, comment: &str) -> [u8; FITS_CARD_SIZE] {
    value_card(keyword, if value { "T" } else { "F" }, comment, true)
}

fn integer_card(
    keyword: &str,
    value: impl std::fmt::Display,
    comment: &str,
) -> [u8; FITS_CARD_SIZE] {
    value_card(keyword, &value.to_string(), comment, true)
}

fn float_card(keyword: &str, value: f64, comment: &str) -> [u8; FITS_CARD_SIZE] {
    value_card(keyword, &format!("{value:.12E}"), comment, true)
}

fn string_card(keyword: &str, value: &str, comment: &str) -> [u8; FITS_CARD_SIZE] {
    let escaped = value.replace('\'', "''");
    value_card(keyword, &format!("'{escaped}'"), comment, false)
}

fn value_card(
    keyword: &str,
    value: &str,
    comment: &str,
    right_aligned: bool,
) -> [u8; FITS_CARD_SIZE] {
    debug_assert!(keyword.len() <= 8);
    let mut text = if right_aligned {
        format!("{keyword:<8}= {value:>20}")
    } else {
        format!("{keyword:<8}= {value:<20}")
    };
    if !comment.is_empty() {
        text.push_str(" / ");
        text.push_str(comment);
    }
    make_card(&text)
}

fn end_card() -> [u8; FITS_CARD_SIZE] {
    make_card("END")
}

fn make_card(text: &str) -> [u8; FITS_CARD_SIZE] {
    let mut card = [b' '; FITS_CARD_SIZE];
    let bytes = text.as_bytes();
    let copy_len = bytes.len().min(FITS_CARD_SIZE);
    card[..copy_len].copy_from_slice(&bytes[..copy_len]);
    card
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

    fn header_size(bytes: &[u8], offset: usize) -> usize {
        let mut card_offset = offset;
        loop {
            if &bytes[card_offset..card_offset + 3] == b"END" {
                let used = card_offset + FITS_CARD_SIZE - offset;
                return used.div_ceil(FITS_BLOCK_SIZE) * FITS_BLOCK_SIZE;
            }
            card_offset += FITS_CARD_SIZE;
        }
    }

    #[test]
    fn writes_padded_big_endian_binary_table() {
        let path =
            std::env::temp_dir().join(format!("hi_observer_fits_test_{}.fits", std::process::id()));
        let values = [1.25_f32, 2.5_f32, 5.0_f32];
        let center_frequency_hz = 1_420_405_751.0;
        let mut writer = FitsTableWriter::create(&path, metadata(values.len())).unwrap();
        writer.write_spectrum(center_frequency_hz, &values).unwrap();
        writer.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() % FITS_BLOCK_SIZE, 0);
        assert_eq!(&bytes[..8], b"SIMPLE  ");

        let primary_size = header_size(&bytes, 0);
        assert_eq!(&bytes[primary_size..primary_size + 8], b"XTENSION");
        let extension_size = header_size(&bytes, primary_size);
        let extension_header = &bytes[primary_size..primary_size + extension_size];
        assert!(extension_header.windows(30).any(|window| {
            String::from_utf8_lossy(window).contains("NAXIS2  =                    1")
        }));

        let data_offset = primary_size + extension_size;
        let stored_frequency =
            f64::from_be_bytes(bytes[data_offset + 8..data_offset + 16].try_into().unwrap());
        assert_eq!(stored_frequency, center_frequency_hz);
        for (index, expected) in values.iter().enumerate() {
            let start = data_offset + 16 + index * 4;
            let stored = f32::from_be_bytes(bytes[start..start + 4].try_into().unwrap());
            assert_eq!(stored, *expected);
        }

        std::fs::remove_file(path).unwrap();
    }
}
