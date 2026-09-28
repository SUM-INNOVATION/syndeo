//! No memory that held a passphrase is given back unwiped while a keystore
//! message is framed, sent, received and dropped, however long the passphrase.
//!
//! This binary's allocator looks inside every block freed while the check is
//! armed, reallocations included, for the passphrase's marker. Anything it
//! finds was freed with the passphrase still in it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use syndeo_ipc::{Framed, KeystoreRequest, SecretString};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const MARKER: &[u8] = b"Wq7-passphrase-marker-9Zx";

static ARMED: AtomicBool = AtomicBool::new(false);
static FOUND: AtomicUsize = AtomicUsize::new(0);

struct Watching;

unsafe impl GlobalAlloc for Watching {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        System.alloc(layout)
    }

    // `realloc` is left to its default, which allocates, copies and calls
    // this: so the block a reallocation leaves behind is looked at too.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::SeqCst) {
            let freed = std::slice::from_raw_parts(ptr, layout.size());
            if freed.windows(MARKER.len()).any(|w| w == MARKER) {
                FOUND.fetch_add(1, Ordering::SeqCst);
            }
        }
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: Watching = Watching;

/// A stream that keeps what is written to it in space reserved up front, so
/// it never frees a block that held the passphrase, and reads from `input`.
struct Recorder {
    written: Vec<u8>,
    input: std::io::Cursor<Vec<u8>>,
}

impl AsyncWrite for Recorder {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        assert!(
            self.written.len() + bytes.len() <= self.written.capacity(),
            "the recorder would reallocate"
        );
        self.written.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for Recorder {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let input = &mut self.input;
        let at = input.position() as usize;
        let rest = &input.get_ref()[at..];
        let n = rest.len().min(out.remaining());
        out.put_slice(&rest[..n]);
        input.set_position((at + n) as u64);
        Poll::Ready(Ok(()))
    }
}

fn armed<T>(run: impl FnOnce() -> T) -> (T, usize) {
    FOUND.store(0, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    let result = run();
    ARMED.store(false, Ordering::SeqCst);
    (result, FOUND.load(Ordering::SeqCst))
}

#[test]
fn a_passphrase_longer_than_the_first_buffer_is_never_freed_unwiped() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    // The check itself works: a plain Vec freed with the marker is caught.
    let ((), caught) = armed(|| drop(MARKER.to_vec()));
    assert!(caught > 0, "the allocator is not looking");

    // Well past the 4 KiB a frame starts in, so encoding it has to grow.
    let passphrase = String::from_utf8(MARKER.repeat(4_000)).unwrap();
    assert!(passphrase.len() > 16 * 4 * 1024);
    let request = KeystoreRequest::Unseal {
        passphrase: Some(SecretString::new(passphrase)),
    };

    let mut framed = Framed::new(Recorder {
        written: Vec::with_capacity(1 << 20),
        input: std::io::Cursor::new(Vec::new()),
    });
    let (sent, freed) = armed(|| runtime.block_on(framed.send(&request)));
    sent.unwrap();
    assert_eq!(
        freed, 0,
        "sending freed {freed} block(s) holding the passphrase"
    );

    // The bytes on the wire are what serde_json writes for the message, after
    // its length, exactly as before.
    let written = std::mem::take(&mut framed.into_inner().written);
    let expected = serde_json::to_vec(&request).unwrap();
    assert_eq!(&written[..4], &(expected.len() as u32).to_be_bytes());
    assert_eq!(&written[4..], &expected[..]);
    drop(expected);

    // And receiving it, and dropping what was received, frees nothing
    // holding it either.
    let mut framed = Framed::new(Recorder {
        written: Vec::new(),
        input: std::io::Cursor::new(written),
    });
    let (received, freed) = armed(|| {
        let received = runtime.block_on(framed.recv::<KeystoreRequest>());
        let unsealed = matches!(
            &received,
            Ok(KeystoreRequest::Unseal { passphrase: Some(p) }) if p.expose().len() == MARKER.len() * 4_000
        );
        drop(received);
        unsealed
    });
    assert!(received, "the passphrase did not arrive whole");
    assert_eq!(
        freed, 0,
        "receiving freed {freed} block(s) holding the passphrase"
    );
}
