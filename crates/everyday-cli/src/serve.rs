//! `everyday serve`: the headless half of "one vault, many windows", and
//! the QR code a pairing link is drawn as for a terminal that has no screen
//! to show a picture on.

use crate::mcp_pipe::{command_error, serve_mcp};
use everyday_core::{Error, Result, Vault};

pub struct ServeOptions<'a> {
    /// Address for the vault server itself.
    pub listen: &'a str,
    pub port: u16,
    /// Print a pairing link and a QR code before serving.
    pub pair: bool,
    pub no_remote_unlock: bool,
    pub no_tls: bool,
    /// Serve the MCP endpoint too. See the flag's own documentation for why
    /// this is not read from `mcp.json`.
    pub mcp: bool,
    pub mcp_listen: Option<&'a str>,
}

pub(crate) fn serve(
    vault: Vault,
    vault_path: &std::path::Path,
    options: ServeOptions<'_>,
) -> Result<()> {
    let ServeOptions { listen, port, pair, no_remote_unlock, no_tls, mcp, mcp_listen } = options;
    let ip: std::net::IpAddr =
        listen.parse().map_err(|_| Error::Invalid(format!("{listen} is not an address")))?;
    let config = everyday_server::Config {
        listen: std::net::SocketAddr::new(ip, port),
        enabled: true,
        allow_remote_unlock: !no_remote_unlock,
        no_tls,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Invalid(format!("could not start a runtime: {e}")))?;

    runtime.block_on(async move {
        everyday_server::install_crypto_provider();
        let dir = everyday_vault::config_dir();
        let parts = everyday_server::prepare(&dir, &config).map_err(command_error)?;
        let registry = parts.registry.clone();
        let broadcaster = parts.broadcaster.clone();

        let name = vault.status().name;
        let locked = !vault.is_unlocked();
        let service = std::sync::Arc::new(everyday_service::Service::new());
        service.set(vault);
        service.set_events(broadcaster.clone());

        let running = everyday_server::start(service.clone(), parts, &config, name.clone())
            .await
            .map_err(command_error)?;

        // MCP, on the same runtime and -- this is the part that matters --
        // the same `Registry`. Two of those over one `devices.json` write
        // the whole list each and silently erase each other's rows; see
        // `Registry`'s own doc.
        let mcp_running = if mcp {
            Some(serve_mcp(&dir, mcp_listen, service.clone(), registry.clone()).await?)
        } else {
            let config = everyday_server::mcp::Config::load(&dir);
            if config.enabled {
                // The switch in the settings panel is on, and this process is
                // deliberately not reading it. Saying so is the whole point:
                // a field that is quietly ignored by one of its two readers
                // is worse than one that is not there.
                eprintln!(
                    "note: mcp.json has the MCP server switched on. This command does not \
                     act on that switch;\n      pass --mcp to serve it here."
                );
            }
            None
        };
        if let Some(running) = &mcp_running {
            // Both sinks, so `lock_state` reaches every paired device *and*
            // every open MCP stream -- which is what tells a client that
            // connected to a locked vault to ask for the catalogue again.
            service
                .set_events(everyday_server::fanout(running.server.clone(), broadcaster.clone()));
            println!("MCP endpoint: http://{}/mcp", running.address);
        }

        // The assistant's routines, on this runtime rather than one of their
        // own. This is the shape the feature was built for: a machine under a
        // desk, no window anywhere, and a seven o'clock brief that happens
        // anyway. It does nothing until the first client unlocks the vault.
        let (stop_scheduler, listen) = tokio::sync::watch::channel(false);
        let scheduling = tokio::spawn(everyday_service::scheduler::run(service.clone(), listen));

        // The local socket as well, always. It is how `everyday new` writes
        // through a running server instead of coming up read-only beside it,
        // and how a browser-extension host will reach a vault that is not
        // shared on any network at all.
        #[cfg(unix)]
        let _socket = {
            // Named after the vault, not after the working directory. Two
            // `serve` processes on different vaults would otherwise collide on
            // one socket -- and any client looking the path up by the vault it
            // wants would find nothing there.
            let path = everyday_server::socket_path(vault_path);
            match everyday_server::serve_socket(running.server.clone(), path.clone()).await {
                Ok(stop) => {
                    println!("Local socket:  {}", path.display());
                    Some(stop)
                }
                Err(e) => {
                    eprintln!("warning: no local socket ({e})");
                    None
                }
            }
        };

        println!("Serving {name} on {}", running.address);
        if locked {
            println!("The vault is locked. The first client to connect can unlock it.");
        }

        if pair {
            let code = registry.new_pairing_code();
            let host = if running.address.ip().is_unspecified() {
                match everyday_server::tls::interface_addresses().first() {
                    Some(ip) => format!("{ip}:{}", running.address.port()),
                    None => format!("127.0.0.1:{}", running.address.port()),
                }
            } else {
                running.address.to_string()
            };
            let invitation =
                everyday_server::pairing::invitation(&host, running.fingerprint(), &code, &name);
            println!();
            println!("{}", terminal_qr(&invitation.url));
            println!("{}", invitation.url);
            println!();
            println!("Good once, for five minutes.");
        }

        println!("Press Ctrl-C to stop.");
        tokio::signal::ctrl_c().await.ok();
        println!();
        println!("Stopping.");
        // The scheduler first, and genuinely waited on. A routine mid-run is
        // spending money and holding the vault's writer, and locking
        // underneath it would fail its next tool call and strand its row
        // saying `Running`. The loop checks the flag between ticks, so this
        // returns as soon as the current tick does and immediately if none is
        // in flight.
        //
        // Not capped. A run has its own fifteen-minute timeout, which bounds
        // this, and a second Ctrl-C is how somebody says they meant it.
        let _ = stop_scheduler.send(true);
        if !scheduling.is_finished() {
            println!("Waiting for the assistant to finish what it was doing…");
        }
        tokio::select! {
            _ = scheduling => {}
            _ = tokio::signal::ctrl_c() => {
                println!("Stopping anyway. A run in flight will not be written down.");
            }
        }
        running.stop();
        // Give the vault its checkpoint before the process goes.
        if let Some(v) = service.get() {
            let _ = v.with_store(|s| s.flush());
            v.lock();
        }
        Ok(())
    })
}

/// A QR code drawn with half-block characters.
///
/// Two rows of the code per line, because a terminal cell is about twice as
/// tall as it is wide and a code drawn one row per line comes out stretched
/// enough that some scanners refuse it. Light on dark, with a quiet zone.
fn terminal_qr(url: &str) -> String {
    use qrcode::{Color, EcLevel, QrCode};
    let Ok(code) = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M) else {
        return String::new();
    };
    let width = code.width();
    let quiet = 2;
    let side = width + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        if x < quiet || y < quiet || x >= width + quiet || y >= width + quiet {
            return false;
        }
        code[(x - quiet, y - quiet)] == Color::Dark
    };

    let mut out = String::new();
    for row in (0..side).step_by(2) {
        for x in 0..side {
            let top = dark(x, row);
            let bottom = row + 1 < side && dark(x, row + 1);
            // A dark module is drawn light: a terminal is usually dark, and a
            // scanner wants the *quiet zone* to be the lighter of the two.
            out.push(match (top, bottom) {
                (true, true) => ' ',
                (true, false) => '\u{2584}',
                (false, true) => '\u{2580}',
                (false, false) => '\u{2588}',
            });
        }
        out.push('\n');
    }
    out
}
