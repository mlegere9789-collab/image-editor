//! `image-editor-server`: the self-hosted backend behind the desktop app's
//! Cloud Documents, Search Your Cloud Files, Invite to Edit and Share for
//! Review rows. One binary, one data directory, no database: documents
//! are stored as versioned files, the index as JSON, tokens as SHA-256
//! hashes. See `server/README.md` for the HTTP contract.
//!
//! ```text
//! image-editor-server [--data-dir DIR] [--listen ADDR]
//! ```
//!
//! On first start it creates the admin token (written to `admin.token`
//! in the data directory, and printed once) and a first user, `owner`,
//! whose token is printed once and stored only as a hash.

mod api;
mod assist;
mod fonts;
mod segment;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::store::Store;

struct Args {
    data_dir: PathBuf,
    listen: SocketAddr,
}

fn parse_args() -> Result<Args, String> {
    let mut data_dir = PathBuf::from("image-editor-data");
    let mut listen: SocketAddr = "127.0.0.1:8787".parse().expect("static address");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = PathBuf::from(args.next().ok_or("--data-dir needs a path")?);
            }
            "--listen" => {
                let value = args.next().ok_or("--listen needs an address")?;
                listen = value
                    .parse()
                    .map_err(|e| format!("--listen {value}: {e}"))?;
            }
            "--help" | "-h" => {
                return Err("usage: image-editor-server [--data-dir DIR] [--listen ADDR]".into());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args { data_dir, listen })
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let (store, first_run) = match Store::open(&args.data_dir) {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("could not open {}: {message}", args.data_dir.display());
            std::process::exit(1);
        }
    };
    if let Some(tokens) = first_run {
        println!("First start. These tokens are shown once; only their hashes are stored.");
        println!("  admin token:         {}", tokens.admin_token);
        println!("  user \"owner\" token:  {}", tokens.owner_token);
        println!(
            "The admin token is also in {} for creating more users.",
            args.data_dir.join("admin.token").display()
        );
    }
    let app = api::router(store, args.data_dir.clone());
    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("could not listen on {}: {e}", args.listen);
            std::process::exit(1);
        }
    };
    println!("image-editor-server listening on http://{}", args.listen);
    println!("Point Edit > External Services > Cloud Documents Endpoint at that URL.");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}
