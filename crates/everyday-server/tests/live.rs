//! Against a server started the way the command line starts one.
//!
//! Everything else in this crate's tests builds a server in-process. This one
//! checks the shape a self-hoster actually gets: `everyday serve`, a pairing
//! link off the terminal, and a client that has only that string to go on.
//!
//! Ignored by default, because it wants the `everyday` binary built. Run it
//! with `cargo test -p everyday-server -- --ignored`.

use everyday_server::client::RemoteClient;
use everyday_server::pairing;

#[tokio::test]
#[ignore = "wants the everyday binary; run with --ignored"]
async fn a_client_pairs_with_a_server_started_from_the_command_line() {
    everyday_server::install_crypto_provider();

    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let binary =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/everyday");
    assert!(binary.exists(), "build it first: cargo build -p everyday-cli");

    let ok = std::process::Command::new(&binary)
        .args(["-C", vault.to_str().unwrap(), "init", "--name", "Served"])
        .env("EVERYDAY_PASSWORD", "a test password here")
        .output()
        .unwrap();
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));

    let log = dir.path().join("serve.log");
    let mut server = std::process::Command::new(&binary)
        .args([
            "-C",
            vault.to_str().unwrap(),
            "serve",
            "--listen",
            "127.0.0.1",
            "--port",
            "7412",
            "--pair",
        ])
        .env("EVERYDAY_PASSWORD", "a test password here")
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Wait for the link to appear in what it printed.
    let mut link = String::new();
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&log)
            && let Some(at) = text.find("everyday://pair?")
        {
            link = text[at..].lines().next().unwrap_or("").to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!link.is_empty(), "the server printed no pairing link");

    let invite = pairing::parse(&link).unwrap();
    let paired = RemoteClient::pair(&invite, "A test client").await;
    let outcome = match paired {
        Ok((client, _token)) => {
            let ctx = everyday_service::Ctx::local();
            client.call(&ctx, "list_journals", serde_json::json!({})).await
        }
        Err(e) => Err(e),
    };
    let _ = server.kill();
    let _ = server.wait();

    let journals = outcome.expect("a paired client must be able to read the vault");
    assert!(journals.is_array(), "{journals}");
}
