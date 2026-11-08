//! Interactive shells (PTY over SSH). Output is pushed to the UI as `term-output` events.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use russh::ChannelMsg;
use sshos_shared::{TermExit, TermOutput};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex};

use crate::error::{AppError, AppResult};
use crate::ssh::Session;

pub enum TermCmd {
    Data(Vec<u8>),
    Resize(u32, u32),
    Close,
}

#[derive(Default)]
pub struct Terminals {
    map: Mutex<HashMap<String, mpsc::UnboundedSender<TermCmd>>>,
}

impl Terminals {
    pub async fn open(
        self: &Arc<Self>,
        app: AppHandle,
        session: Arc<Session>,
        cols: u32,
        rows: u32,
        id: String,
        initial_input: Option<String>,
    ) -> AppResult<String> {
        let ch = session.handle.channel_open_session().await?;
        ch.request_pty(true, "xterm-256color", cols.max(10), rows.max(4), 0, 0, &[]).await?;
        ch.set_env(false, "COLORTERM", "truecolor").await.ok();
        ch.request_shell(true).await?;
        let (mut reader, writer) = ch.split();

        let (tx, mut rx) = mpsc::unbounded_channel::<TermCmd>();
        self.map.lock().await.insert(id.clone(), tx.clone());

        // Pre-typed command (never followed by Enter: the user presses it).
        if let Some(cmd) = initial_input.filter(|c| !c.is_empty()) {
            let _ = tx.send(TermCmd::Data(cmd.into_bytes()));
        }

        // Writer task.
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let res = match cmd {
                    TermCmd::Data(d) => writer.data(&d[..]).await,
                    TermCmd::Resize(c, r) => writer.window_change(c, r, 0, 0).await,
                    TermCmd::Close => {
                        let _ = writer.eof().await;
                        let _ = writer.close().await;
                        break;
                    }
                };
                if res.is_err() {
                    break;
                }
            }
        });

        // Reader task.
        let me = self.clone();
        let tid = id.clone();
        tokio::spawn(async move {
            let b64 = base64::engine::general_purpose::STANDARD;
            let mut code = None;
            while let Some(msg) = reader.wait().await {
                match msg {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        let _ = app.emit("term-output", TermOutput { id: tid.clone(), data: b64.encode(&data) });
                    }
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            me.map.lock().await.remove(&tid);
            let _ = app.emit("term-exit", TermExit { id: tid, code });
        });

        Ok(id)
    }

    pub async fn send(&self, id: &str, cmd: TermCmd) -> AppResult<()> {
        let map = self.map.lock().await;
        let tx = map.get(id).ok_or_else(|| AppError::msg("Terminal closed"))?;
        tx.send(cmd).map_err(|_| AppError::msg("Terminal closed"))
    }
}
