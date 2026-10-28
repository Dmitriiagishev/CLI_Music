use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};
pub const BANDS: usize = 20;
const FREQUENCIES: [u32; BANDS] = [
    50, 70, 100, 140, 200, 280, 400, 560, 800, 1100, 1500, 2100, 2900, 4000, 5500, 7500, 10000,
    12000, 14000, 16000,
];
pub struct Spectrum {
    _directory: tempfile::TempDir,
    files: Vec<PathBuf>,
}
impl Spectrum {
    pub fn new() -> std::io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("cli-music-spectrum-")
            .tempdir()?;
        let files = (0..BANDS)
            .map(|index| directory.path().join(format!("band{index}")))
            .collect();
        Ok(Self {
            _directory: directory,
            files,
        })
    }
    pub fn graph(&self) -> Option<String> {
        let mut graph = format!("asplit={}[main]", BANDS + 1);
        for index in 0..BANDS {
            graph.push_str(&format!("[s{index}]"));
        }
        for (index, frequency) in FREQUENCIES.iter().enumerate() {
            let path = self.files[index].to_str()?;
            // FFmpeg's graph parser uses these characters for quoting/escaping.
            if path
                .chars()
                .any(|ch| matches!(ch, '\'' | '\\' | ':' | ']' | '[' | ';' | ','))
            {
                return None;
            }
            graph.push_str(&format!(";[s{index}]bandpass=f={frequency}:width_type=o:width=1,asetnsamples=n=8192:p=0,astats=metadata=1:reset=1,ametadata=mode=print:key=lavfi.astats.Overall.RMS_level:file='{path}':direct=1,anullsink"));
        }
        graph.push_str(";[main]anull");
        Some(graph)
    }
    pub fn levels(&self) -> [f32; BANDS] {
        let mut values = [0.0; BANDS];
        for (index, path) in self.files.iter().enumerate() {
            if let Ok(mut file) = File::open(path) {
                let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                if file
                    .seek(SeekFrom::Start(len.saturating_sub(1024)))
                    .is_err()
                {
                    continue;
                }
                let mut tail = String::new();
                if file.read_to_string(&mut tail).is_err() {
                    continue;
                }
                if let Some(level) = tail.lines().rev().find_map(|line| {
                    line.strip_prefix("lavfi.astats.Overall.RMS_level=")
                        .and_then(|v| v.parse::<f32>().ok())
                }) {
                    if level.is_finite() {
                        values[index] = ((level + 60.0) / 50.0).clamp(0.0, 1.0);
                    }
                }
            }
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_levels_reflect_frequency_metadata_and_silence() {
        let spectrum = Spectrum::new().unwrap();
        std::fs::write(&spectrum.files[0], "frame:0\nlavfi.astats.Overall.RMS_level=-55\nframe:1\nlavfi.astats.Overall.RMS_level=-50\n").unwrap();
        std::fs::write(&spectrum.files[8], "lavfi.astats.Overall.RMS_level=-10\n").unwrap();
        std::fs::write(&spectrum.files[7], "lavfi.astats.Overall.RMS_level=-inf\n").unwrap();
        let levels = spectrum.levels();
        assert!((levels[0] - 0.2).abs() < 0.001);
        assert_eq!(levels[8], 1.0);
        assert_eq!(levels[7], 0.0);
    }
}
