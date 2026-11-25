//! Off-thread, debounced persistence for the profile store.
//!
//! Saving used to serialize the whole store and fsync it while the Slint thread
//! held the store lock, which made selection and settings changes visibly lag
//! with large subscription groups. Writes now go to a dedicated thread that
//! coalesces bursts into one write.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;

use bamdad_core::config::{save_store, Store};

enum Message {
    Store(Store),
    Flush(Sender<()>),
}

static SAVER: OnceLock<Sender<Message>> = OnceLock::new();

fn sender() -> &'static Sender<Message> {
    SAVER.get_or_init(|| {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("bamdad-store-saver".into())
            .spawn(move || run(rx));
        tx
    })
}

fn run(rx: Receiver<Message>) {
    while let Ok(message) = rx.recv() {
        let mut latest = match message {
            Message::Store(store) => store,
            Message::Flush(ack) => {
                let _ = ack.send(());
                continue;
            }
        };

        // Let a burst (selection, ping reorders, several setting toggles)
        // settle into a single write.
        std::thread::sleep(Duration::from_millis(120));
        let mut waiters = Vec::new();
        while let Ok(next) = rx.try_recv() {
            match next {
                Message::Store(store) => latest = store,
                Message::Flush(ack) => waiters.push(ack),
            }
        }

        if let Err(error) = save_store(&latest) {
            eprintln!("bamdad: could not save profiles: {error}");
        }
        for ack in waiters {
            let _ = ack.send(());
        }
    }
}

/// Queues a save. Cheap: the caller only clones the in-memory store.
pub fn save(store: &Store) {
    let _ = sender().send(Message::Store(store.clone()));
}

/// Blocks until everything queued so far is on disk (used on shutdown).
pub fn flush() {
    if let Some(tx) = SAVER.get() {
        let (ack_tx, ack_rx) = channel();
        if tx.send(Message::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(3));
        }
    }
}
