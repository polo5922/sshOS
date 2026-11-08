//! File operations over SFTP (+ a few shell commands where SFTP v3 is too limited).

use std::sync::Arc;

use russh_sftp::client::SftpSession;
use sshos_shared::{FileContent, FileEntry, TransferProgress, WriteResult};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{AppError, AppResult};
use crate::ssh::{sh_quote, Session};

const MAX_EDIT_SIZE: u64 = 8 * 1024 * 1024;

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

pub fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((p, _)) => p.into(),
    }
}

pub async fn list(sftp: &SftpSession, path: &str) -> AppResult<Vec<FileEntry>> {
    let path = sftp.canonicalize(path).await?;
    let mut out = Vec::new();
    for e in sftp.read_dir(&path).await? {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let m = e.metadata();
        let full = join(&path, &name);
        let mut is_dir = m.is_dir();
        let is_link = m.is_symlink();
        if is_link {
            // Follow the link to know whether it points to a directory.
            if let Ok(t) = sftp.metadata(&full).await {
                is_dir = t.is_dir();
            }
        }
        out.push(FileEntry {
            name,
            path: full,
            is_dir,
            is_link,
            size: m.size.unwrap_or(0),
            mtime: m.mtime.unwrap_or(0),
            mode: m.permissions.unwrap_or(0),
            user: m.user.clone().or(m.uid.map(|u| u.to_string())).unwrap_or_default(),
            group: m.group.clone().or(m.gid.map(|g| g.to_string())).unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

pub async fn read_text(sftp: &SftpSession, path: &str) -> AppResult<FileContent> {
    let meta = sftp.metadata(path).await?;
    let size = meta.size.unwrap_or(0);
    if size > MAX_EDIT_SIZE {
        return Err(AppError::msg(format!("{path} is too large to edit ({size} bytes)")));
    }
    let bytes = sftp.read(path).await?;
    let (text, binary) = match String::from_utf8(bytes) {
        Ok(s) => (s, false),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
    };
    Ok(FileContent { path: path.into(), text, binary, size })
}

/// Atomic save: write a temp file next to the target, keep mode/owner, then `mv`.
/// Files under /etc get a timestamped backup first (like Davish).
/// If the user cannot write there, the content is staged in /tmp and a `sudo` command is returned
/// for the UI to type into a terminal.
pub async fn write_text(
    session: &Session,
    sftp: &SftpSession,
    path: &str,
    text: &str,
) -> AppResult<WriteResult> {
    let exists = sftp.try_exists(path).await.unwrap_or(false);
    let ts = session.exec_ok("date +%Y%m%d-%H%M%S").await.unwrap_or_default();
    let backup = (exists && path.starts_with("/etc/")).then(|| format!("{path}.bak-{}", ts.trim()));
    let name = path.rsplit('/').next().unwrap_or("file");
    let tmp = join(&parent(path), &format!(".{name}.sshos-{}", uuid::Uuid::new_v4().simple()));

    match write_create(sftp, &tmp, text.as_bytes()).await {
        Ok(()) => {
            let q = sh_quote(path);
            let qt = sh_quote(&tmp);
            let mut cmd = String::new();
            if let Some(b) = &backup {
                cmd += &format!("cp -p {q} {} && ", sh_quote(b));
            }
            if exists {
                cmd += &format!(
                    "(chmod --reference={q} {qt} 2>/dev/null; chown --reference={q} {qt} 2>/dev/null; true) && "
                );
            }
            cmd += &format!("mv -f {qt} {q}");
            if let Err(e) = session.exec_ok(&cmd).await {
                let _ = sftp.remove_file(&tmp).await;
                return Err(e);
            }
            Ok(WriteResult { path: path.into(), backup })
        }
        Err(_) => {
            // Permission denied: stage in /tmp and hand a sudo command to the terminal.
            let staged = format!("/tmp/sshos-{}", uuid::Uuid::new_v4().simple());
            write_create(sftp, &staged, text.as_bytes()).await?;
            let q = sh_quote(path);
            let mut inner = String::new();
            if let Some(b) = &backup {
                inner += &format!("cp -p {q} {} && ", sh_quote(b));
            }
            // `cat >` keeps the owner and mode of the existing file.
            inner += &format!("cat {staged} > {q} && rm -f {staged}");
            Err(AppError::needs_terminal(format!("sudo sh -c {}", sh_quote(&inner))))
        }
    }
}

async fn write_create(sftp: &SftpSession, path: &str, data: &[u8]) -> AppResult<()> {
    let mut f = sftp.create(path).await?;
    f.write_all(data).await?;
    f.flush().await?;
    f.shutdown().await?;
    Ok(())
}

pub async fn upload(
    app: &AppHandle,
    sftp: Arc<SftpSession>,
    local: &str,
    remote_dir: &str,
) -> AppResult<String> {
    let name = std::path::Path::new(local)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::msg("Bad local path"))?
        .to_string();
    let remote = join(remote_dir, &name);
    let mut src = tokio::fs::File::open(local).await?;
    let total = src.metadata().await?.len();
    let mut dst = sftp.create(&remote).await?;
    copy_with_progress(app, &name, &mut src, &mut dst, total).await?;
    dst.shutdown().await?;
    Ok(remote)
}

pub async fn download(app: &AppHandle, sftp: Arc<SftpSession>, remote: &str, local: &str) -> AppResult<()> {
    let name = remote.rsplit('/').next().unwrap_or(remote).to_string();
    let total = sftp.metadata(remote).await?.size.unwrap_or(0);
    let mut src = sftp.open(remote).await?;
    let mut dst = tokio::fs::File::create(local).await?;
    copy_with_progress(app, &name, &mut src, &mut dst, total).await?;
    dst.flush().await?;
    Ok(())
}

async fn copy_with_progress<R, W>(app: &AppHandle, name: &str, src: &mut R, dst: &mut W, total: u64) -> AppResult<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let id = uuid::Uuid::new_v4().to_string();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = std::time::Instant::now();
    let emit = |done, finished, error: Option<String>| {
        let _ = app.emit(
            "transfer-progress",
            TransferProgress { id: id.clone(), name: name.into(), done, total, finished, error },
        );
    };
    loop {
        let n = match src.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                emit(done, true, Some(e.to_string()));
                return Err(e.into());
            }
        };
        if n == 0 {
            break;
        }
        if let Err(e) = dst.write_all(&buf[..n]).await {
            emit(done, true, Some(e.to_string()));
            return Err(e.into());
        }
        done += n as u64;
        if last.elapsed().as_millis() > 150 {
            emit(done, false, None);
            last = std::time::Instant::now();
        }
    }
    emit(done, true, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("/etc", "nginx"), "/etc/nginx");
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(parent("/etc/nginx/nginx.conf"), "/etc/nginx");
        assert_eq!(parent("/etc"), "/");
    }
}
