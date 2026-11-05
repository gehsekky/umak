//! Where audio bytes come from: a local file now, an HTTP stream later.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use symphonia::core::io::MediaSource;

pub trait AudioSource: Send {
    fn open(&self) -> io::Result<Box<dyn MediaSource>>;
    /// A hint for choosing the demuxer.
    fn extension(&self) -> Option<String>;
}

pub struct LocalFile(pub PathBuf);

impl AudioSource for LocalFile {
    fn open(&self) -> io::Result<Box<dyn MediaSource>> {
        Ok(Box::new(File::open(&self.0)?))
    }

    fn extension(&self) -> Option<String> {
        self.0
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
    }
}
