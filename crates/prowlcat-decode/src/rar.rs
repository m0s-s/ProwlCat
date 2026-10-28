//! RAR archives, read through an external program (`unrar`, or 7-Zip).
//!
//! RAR's decoder is not free software under terms that fit this project's license, so ProwlCat
//! does not contain one. It runs the program you have installed, as a separate process, and reads
//! what it prints: the archive is listed first (names, sizes), then every member is streamed in
//! one pass and cut at the listed sizes, so members keep their names and nothing is written to
//! disk. The program is never run through a shell, and the archive path comes after `--`.
//!
//! Which program: `PROWLCAT_RAR_TOOL` (a path), else the first of `7zz`, `7z` found in `PATH`.
//! `unrar` is never picked by itself; it is used only when `PROWLCAT_RAR_TOOL` names it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use tracing::warn;

use crate::plain::read_lines;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Unrar,
    SevenZip,
}

#[derive(Debug, Clone)]
struct Tool {
    flavor: Flavor,
    program: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    name: String,
    size: u64,
    encrypted: bool,
}

fn flavor_of(program: &Path) -> Flavor {
    let stem = program.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if stem.starts_with("7z") {
        Flavor::SevenZip
    } else {
        Flavor::Unrar
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&file)).find(|p| p.is_file())
}

fn find_tool() -> Option<&'static Tool> {
    static TOOL: OnceLock<Option<Tool>> = OnceLock::new();
    TOOL.get_or_init(|| {
        if let Some(path) = std::env::var_os("PROWLCAT_RAR_TOOL").filter(|p| !p.is_empty()) {
            let program = PathBuf::from(path);
            return Some(Tool { flavor: flavor_of(&program), program });
        }
        ["7zz", "7z"].iter().find_map(|n| on_path(n).map(|program| Tool { flavor: flavor_of(&program), program }))
    })
    .as_ref()
}

/// Whether a program that can read RAR archives is installed.
pub fn rar_tool_available() -> bool {
    find_tool().is_some()
}

pub(crate) const NO_TOOL: &str = "no program to read RAR archives was found: install 7-Zip (`7zz` or `7z`), \
or set PROWLCAT_RAR_TOOL to the path of one";

fn password_arg(flavor: Flavor, password: Option<&str>) -> String {
    match (flavor, password) {
        (_, Some(p)) => format!("-p{p}"),
        (Flavor::Unrar, None) => "-p-".to_string(),
        // 7-Zip has no "no password" switch; with no terminal it fails instead of asking.
        (Flavor::SevenZip, None) => "-p".to_string(),
    }
}

impl Tool {
    fn command(&self, args: &[String], archive: &Path, then: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(args).arg("--").arg(archive).args(then).stdin(Stdio::null()).stderr(Stdio::null());
        cmd
    }

    fn list_cmd(&self, archive: &Path, password: Option<&str>) -> Command {
        let pw = password_arg(self.flavor, password);
        match self.flavor {
            Flavor::Unrar => self.command(&["lt".into(), pw], archive, &[]),
            Flavor::SevenZip => self.command(&["l".into(), "-slt".into(), "-bd".into(), "-y".into(), pw], archive, &[]),
        }
    }

    fn test_cmd(&self, archive: &Path, password: Option<&str>, member: &str) -> Command {
        let pw = password_arg(self.flavor, password);
        match self.flavor {
            Flavor::Unrar => self.command(&["t".into(), "-inul".into(), pw], archive, &[member]),
            Flavor::SevenZip => self.command(&["t".into(), "-bd".into(), "-y".into(), pw], archive, &[member]),
        }
    }

    fn stream_cmd(&self, archive: &Path, password: Option<&str>) -> Command {
        let pw = password_arg(self.flavor, password);
        match self.flavor {
            Flavor::Unrar => self.command(&["p".into(), "-inul".into(), pw], archive, &[]),
            Flavor::SevenZip => self.command(&["x".into(), "-so".into(), "-bd".into(), "-y".into(), pw], archive, &[]),
        }
    }
}

/// A child process that is killed and reaped when dropped, so an aborted scan leaves nothing running.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_capture(mut cmd: Command) -> Result<String> {
    let mut child = Reaper(cmd.stdout(Stdio::piped()).spawn().context("start the RAR program")?);
    let mut out = Vec::new();
    child.0.stdout.take().context("no output pipe")?.read_to_end(&mut out)?;
    let status = child.0.wait()?;
    if !status.success() {
        bail!("the RAR program failed ({status})");
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Parse `unrar lt`: blocks of `Name:`, `Type:`, `Size:`, `Flags:` lines.
fn parse_unrar_list(text: &str) -> Vec<Member> {
    let mut members = Vec::new();
    let (mut name, mut is_file, mut size, mut encrypted): (Option<String>, bool, u64, bool) = (None, false, 0, false);
    let mut flush = |name: &mut Option<String>, is_file: bool, size: u64, encrypted: bool| {
        if let Some(n) = name.take() {
            if is_file {
                members.push(Member { name: n, size, encrypted });
            }
        }
    };
    for line in text.lines() {
        let l = line.trim_start();
        if let Some(n) = l.strip_prefix("Name:") {
            flush(&mut name, is_file, size, encrypted);
            name = Some(n.strip_prefix(' ').unwrap_or(n).to_string());
            (is_file, size, encrypted) = (false, 0, false);
        } else if let Some(t) = l.strip_prefix("Type:") {
            is_file = t.trim().eq_ignore_ascii_case("file");
        } else if let Some(sz) = l.strip_prefix("Size:") {
            size = sz.trim().parse().unwrap_or(0);
        } else if let Some(fl) = l.strip_prefix("Flags:") {
            encrypted = fl.to_ascii_lowercase().contains("encrypted");
        }
    }
    flush(&mut name, is_file, size, encrypted);
    members
}

/// Parse `7z l -slt`: blocks of `Key = value` lines separated by blank lines; the block that
/// describes the archive itself has a `Type` key and is skipped.
fn parse_7z_list(text: &str) -> Vec<Member> {
    let mut members = Vec::new();
    for block in text.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let (mut name, mut folder, mut size, mut encrypted, mut is_archive) = (None, false, 0u64, false, false);
        for line in block.lines() {
            let Some((k, v)) = line.split_once(" = ") else { continue };
            match k.trim() {
                "Path" => name = Some(v.to_string()),
                "Type" => is_archive = true,
                "Folder" => folder = v.trim() == "+",
                "Size" => size = v.trim().parse().unwrap_or(0),
                "Encrypted" => encrypted = v.trim() == "+",
                _ => {}
            }
        }
        if let (Some(name), false, false) = (name, folder, is_archive) {
            members.push(Member { name, size, encrypted });
        }
    }
    members
}

fn list(tool: &Tool, archive: &Path, password: Option<&str>) -> Result<Vec<Member>> {
    let text = run_capture(tool.list_cmd(archive, password))?;
    Ok(match tool.flavor {
        Flavor::Unrar => parse_unrar_list(&text),
        Flavor::SevenZip => parse_7z_list(&text),
    })
}

/// Find how to open the archive: no password if it needs none, else the first candidate that
/// decodes its first encrypted member. Returns the password to stream with and the members.
fn open<'p>(tool: &Tool, archive: &Path, passwords: &'p [String]) -> Result<(Option<&'p str>, Vec<Member>)> {
    let candidates: Vec<Option<&str>> = std::iter::once(None).chain(passwords.iter().map(|p| Some(p.as_str()))).collect();
    let mut last = anyhow!("unreadable");
    for password in candidates {
        let members = match list(tool, archive, password) {
            Ok(m) => m,
            Err(e) => {
                last = e; // encrypted headers, or damage
                continue;
            }
        };
        let Some(first) = members.iter().find(|m| m.encrypted && m.size > 0) else {
            return Ok((password, members));
        };
        match run_capture(tool.test_cmd(archive, password, &first.name)) {
            Ok(_) => return Ok((password, members)),
            Err(e) => last = e,
        }
    }
    Err(last).context("could not open the archive (encrypted: use --password or --password-file; or damaged)")
}

/// Whether the archive can be opened, without reading its data.
pub(crate) fn probe_rar(path: &Path, passwords: &[String]) -> crate::Probe {
    let Some(tool) = find_tool() else { return crate::Probe::NoTool };
    match open(tool, path, passwords) {
        Ok(_) => crate::Probe::Readable,
        Err(e) => match list(tool, path, None) {
            // Listing works, so the members are encrypted and no candidate fitted.
            Ok(members) if members.iter().any(|m| m.encrypted) => crate::Probe::NeedsPassword,
            _ => crate::Probe::Unreadable(format!("{e:#}")),
        },
    }
}

/// Walk a RAR archive with the external program, handing each member's lines to `f`.
pub(crate) fn scan_rar(
    path: &Path,
    passwords: &[String],
    cap: usize,
    f: &mut impl FnMut(Option<&str>, u64, u64, usize, &[u8]),
) -> Result<()> {
    let tool = find_tool().ok_or_else(|| anyhow!(NO_TOOL))?;
    let (password, members) = open(tool, path, passwords).with_context(|| format!("RAR archive {}", path.display()))?;
    if members.iter().all(|m| m.size == 0) {
        return Ok(());
    }

    let mut child = Reaper(tool.stream_cmd(path, password).stdout(Stdio::piped()).spawn().context("start the RAR program")?);
    let mut stdout = child.0.stdout.take().context("no output pipe")?;
    for m in &members {
        let mut data = (&mut stdout).take(m.size);
        read_lines(&mut data, cap, |n, o, k, l| f(Some(&m.name), n, o, k, l))
            .with_context(|| format!("RAR member {}", m.name))?;
        if data.limit() != 0 {
            bail!("the RAR program stopped early in {} ({} of {} bytes)", m.name, m.size - data.limit(), m.size);
        }
    }
    drop(stdout);
    let status = child.0.wait()?;
    if !status.success() {
        warn!(path = %path.display(), %status, "the RAR program reported a problem (damaged archive or wrong password?)");
        bail!("the RAR program failed ({status}) on {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrar_listing() {
        let text = "\nUNRAR 7.30 freeware\n\nArchive: t.rar\nDetails: RAR 5\n\n        Name: a/one.txt\n        Type: File\n        Size: 21\n Packed size: 21\n       Flags: encrypted\n\n        Name: dir\n        Type: Directory\n        Size: 0\n\n        Name: two words.txt\n        Type: File\n        Size: 33\n";
        assert_eq!(
            parse_unrar_list(text),
            vec![
                Member { name: "a/one.txt".into(), size: 21, encrypted: true },
                Member { name: "two words.txt".into(), size: 33, encrypted: false },
            ]
        );
    }

    #[test]
    fn sevenzip_listing() {
        let text = "7-Zip 23.01\n\nListing archive: x.rar\n\n--\nPath = x.rar\nType = Rar\nPhysical Size = 100\n\n----------\nPath = a.txt\nFolder = -\nSize = 12\nEncrypted = -\n\nPath = sub\nFolder = +\nSize = 0\n\nPath = sub/b.txt\nFolder = -\nSize = 7\nEncrypted = +\n";
        assert_eq!(
            parse_7z_list(text),
            vec![
                Member { name: "a.txt".into(), size: 12, encrypted: false },
                Member { name: "sub/b.txt".into(), size: 7, encrypted: true },
            ]
        );
    }

    #[test]
    fn tools_are_told_apart_by_name() {
        assert_eq!(flavor_of(Path::new("/usr/bin/unrar")), Flavor::Unrar);
        assert_eq!(flavor_of(Path::new("/usr/bin/7zz")), Flavor::SevenZip);
        assert_eq!(flavor_of(Path::new("7z.exe")), Flavor::SevenZip);
        assert_eq!(flavor_of(Path::new("UnRAR.exe")), Flavor::Unrar);
    }
}
