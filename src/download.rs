use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};

use crate::apple::{self, AppleMusicUrl};

const USER_AGENT: &str = "sully";

pub(crate) struct Downloader {
    client: Client,
    output_dir: PathBuf,
    force: bool,
    verbose: bool,
}

impl Downloader {
    pub(crate) fn new(output_dir: PathBuf, force: bool, verbose: bool) -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("could not configure HTTP client")?;

        Ok(Self {
            client,
            output_dir,
            force,
            verbose,
        })
    }

    pub(crate) fn run(&self, raw_url: &str) -> Result<()> {
        self.progress(raw_url, "validating Apple Music URL");
        let page_url = AppleMusicUrl::parse(raw_url)?;

        self.progress(raw_url, "fetching Apple Music page");
        let page = self
            .fetch(page_url.as_str())
            .context("could not fetch album page")?;
        let html = page.text().context("could not read album page")?;

        self.progress(raw_url, "reading cover metadata");
        let cover = apple::parse_cover(&html)?;

        let destination = self.output_dir.join(&cover.file_name);
        let display_path = display_path(&self.output_dir, &cover.file_name);
        self.progress(
            raw_url,
            &format!("found cover artwork: {}", cover.file_name),
        );

        if !self.force {
            self.progress(raw_url, "checking whether the cover already exists");
            if destination.exists() {
                eprintln!("sully: {raw_url}: already exists: {display_path}");
                return Ok(());
            }
        }

        self.progress(raw_url, "downloading cover artwork");
        let image = self
            .fetch(cover.image_url.as_str())
            .context("could not download cover")?;
        let bytes = image.bytes().context("could not read cover response")?;

        if bytes.is_empty() {
            anyhow::bail!("cover response was empty");
        }

        self.progress(raw_url, &format!("saving cover to {display_path}"));
        match write_cover(&destination, &bytes, self.force) {
            Ok(()) => {}
            Err(error) if !self.force && error.kind() == std::io::ErrorKind::AlreadyExists => {
                eprintln!("sully: {raw_url}: already exists: {display_path}");
                return Ok(());
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context("could not save cover"));
            }
        }

        println!("{display_path}");
        Ok(())
    }

    fn fetch(&self, url: &str) -> Result<Response> {
        let response = self.client.get(url).send()?;
        let status = response.status();

        if !status.is_success() {
            anyhow::bail!("returned {status}");
        }

        Ok(response)
    }

    fn progress(&self, url: &str, message: &str) {
        if self.verbose {
            eprintln!("sully: {url}: {message}");
        }
    }
}

fn display_path(output_dir: &Path, file_name: &str) -> String {
    if output_dir == Path::new(".") {
        file_name.to_owned()
    } else {
        output_dir.join(file_name).display().to_string()
    }
}

fn write_cover(destination: &Path, bytes: &[u8], replace: bool) -> std::io::Result<()> {
    let directory = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;

    temporary.write_all(bytes)?;
    temporary.flush()?;

    let persisted = if replace {
        temporary.persist(destination)
    } else {
        temporary.persist_noclobber(destination)
    };

    persisted.map(|_| ()).map_err(|error| error.error)
}
