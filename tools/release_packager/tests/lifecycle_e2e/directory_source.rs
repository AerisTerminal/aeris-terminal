//! A release origin served from a packager output tree, recording every request.

use std::{
    cell::RefCell,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    rc::Rc,
};

use aeris_release_lifecycle::{ArtifactBody, ReleaseSource, ReleaseSourceError};

/// One launcher request, by kind and path below the base URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Channel(String),
    Object(String),
    Range(String),
    Artifact(String),
}

pub struct DirectorySource {
    root: PathBuf,
    base_url: String,
    requests: Rc<RefCell<Vec<Request>>>,
}

impl DirectorySource {
    pub fn new(root: &Path, base_url: &str, requests: Rc<RefCell<Vec<Request>>>) -> Self {
        Self {
            root: root.to_path_buf(),
            base_url: base_url.to_string(),
            requests,
        }
    }

    /// Records the request and opens the file the URL names below the root.
    fn open(&self, url: &str, request: fn(String) -> Request) -> Result<File, ReleaseSourceError> {
        let relative = url
            .strip_prefix(&self.base_url)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| ReleaseSourceError::Request(format!("{url} is outside the origin")))?;
        let mut path = self.root.clone();
        for part in relative.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                return Err(ReleaseSourceError::Request(format!(
                    "{url} is not a release path"
                )));
            }
            path.push(part);
        }
        self.requests
            .borrow_mut()
            .push(request(relative.to_string()));
        File::open(path).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ReleaseSourceError::Status(404),
            _ => ReleaseSourceError::Request(error.to_string()),
        })
    }

    fn read_bounded(
        &self,
        url: &str,
        maximum: u64,
        request: fn(String) -> Request,
    ) -> Result<Vec<u8>, ReleaseSourceError> {
        let mut bytes = Vec::new();
        self.open(url, request)?
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ReleaseSourceError::Unreadable)?;
        if bytes.len() as u64 > maximum {
            return Err(ReleaseSourceError::TooLarge);
        }
        Ok(bytes)
    }
}

impl ReleaseSource for DirectorySource {
    fn read_channel(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError> {
        self.read_bounded(url, maximum, Request::Channel)
    }

    fn read_object(&self, url: &str, maximum: u64) -> Result<Vec<u8>, ReleaseSourceError> {
        self.read_bounded(url, maximum, Request::Object)
    }

    fn read_range(
        &self,
        url: &str,
        offset: u64,
        length: u32,
        total: u64,
    ) -> Result<Vec<u8>, ReleaseSourceError> {
        let mut file = self.open(url, Request::Range)?;
        let size = file
            .metadata()
            .map_err(|_| ReleaseSourceError::Unreadable)?
            .len();
        if size != total
            || offset
                .checked_add(u64::from(length))
                .is_none_or(|end| end > total)
        {
            return Err(ReleaseSourceError::RangeMismatch);
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| ReleaseSourceError::Unreadable)?;
        let mut bytes = vec![0_u8; length as usize];
        file.read_exact(&mut bytes)
            .map_err(|_| ReleaseSourceError::Unreadable)?;
        Ok(bytes)
    }

    fn open_artifact(
        &self,
        url: &str,
        offset: u64,
        size: u64,
    ) -> Result<ArtifactBody, ReleaseSourceError> {
        let mut file = self.open(url, Request::Artifact)?;
        let resumed = offset > 0 && offset < size;
        if resumed {
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| ReleaseSourceError::Unreadable)?;
        }
        Ok(ArtifactBody {
            resumed,
            reader: Box::new(file),
        })
    }
}
