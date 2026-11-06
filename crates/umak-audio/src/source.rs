//! Where audio bytes come from: a local file now, an HTTP stream later.

use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

use symphonia::core::io::MediaSource;

/// Lets other programs read, write and replace a file while it is open.
#[cfg(windows)]
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

pub trait AudioSource: Send {
    fn open(&self) -> io::Result<Box<dyn MediaSource>>;
    /// A hint for choosing the demuxer.
    fn extension(&self) -> Option<String>;
}

pub struct LocalFile(pub PathBuf);

impl AudioSource for LocalFile {
    fn open(&self) -> io::Result<Box<dyn MediaSource>> {
        let mut options = OpenOptions::new();
        options.read(true);
        // So a tag edit can replace the file during playback. This handle keeps reading the
        // file as it was, and the next opening finds the new one.
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, SHARE_ALL);
        Ok(Box::new(options.open(&self.0)?))
    }

    fn extension(&self) -> Option<String> {
        self.0
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
    }
}
