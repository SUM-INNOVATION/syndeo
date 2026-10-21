//! Who is at the other end of an agent's connection, as the kernel says.
//!
//! The socket's peer — its user and its process — comes from the kernel,
//! not from anything the peer says: `getpeereid` for the user, and
//! `LOCAL_PEERPID` for the process. From that process up, through each
//! parent, the line of processes that led to it; the agent the person is
//! asked about is chosen from that line (`agent::root_of`), and its
//! signature, as the Security framework reads it, is shown to them.

use crate::agent::{self, Proc};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use std::ffi::c_void;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// The process at the other end of a connection, and the line above it.
#[derive(Debug, Clone)]
pub struct Peer {
    /// The connecting process first, then its parent, and so on, up to and
    /// not including launchd.
    pub chain: Vec<Proc>,
    /// Which of them is the agent.
    pub root: usize,
    /// The agent's signature, in words.
    pub signing: String,
}

impl Peer {
    /// The agent: its process.
    pub fn agent(&self) -> Option<&Proc> {
        self.chain.get(self.root)
    }

    /// The line from the agent down to the connecting process, by name.
    pub fn through(&self) -> String {
        self.chain[..self.root.min(self.chain.len())]
            .iter()
            .rev()
            .map(|proc| {
                proc.path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&proc.path)
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

/// The user the connected process runs as, as the kernel has it.
pub fn uid_of(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: a connected socket, and two out-parameters of the right type.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

/// The connected process, as the kernel has it.
pub fn pid_of(stream: &UnixStream) -> io::Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut size = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: a connected socket, and an out-parameter of the size given.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut size,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(pid).map_err(|_| io::Error::other("no peer process"))
}

/// A process's parent and when it started; `None` once it is gone.
fn info(pid: u32) -> Option<(u32, (u64, u64))> {
    // SAFETY: zeroes are a valid proc_bsdinfo, which proc_pidinfo fills.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (got == size).then_some((info.pbi_ppid, (info.pbi_start_tvsec, info.pbi_start_tvusec)))
}

/// A process's executable, as the kernel has it, or empty.
fn path_of(pid: u32) -> String {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: a buffer of the size given.
    let n = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if n <= 0 {
        return String::new();
    }
    buffer.truncate(n as usize);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// The line from `pid` up to, and not including, launchd: at most 24.
pub fn chain(pid: u32) -> Vec<Proc> {
    let mut chain = Vec::new();
    let mut next = pid;
    while next > 1 && chain.len() < 24 {
        let Some((parent, started)) = info(next) else {
            break;
        };
        chain.push(Proc {
            pid: next,
            path: path_of(next),
            started,
        });
        next = parent;
    }
    chain
}

/// Everything about the process at the other end of `stream` that the
/// person is shown; `own` is this executable's path, which a client of
/// Syndeo's own runs.
pub fn describe(pid: u32, own: &str) -> Peer {
    let chain = chain(pid);
    let root = agent::root_of(&chain, own);
    let signing = chain
        .get(root)
        .map(|agent| signing_of(agent.pid))
        .unwrap_or_else(|| "unknown".to_string());
    Peer {
        chain,
        root,
        signing,
    }
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecGuestAttributePid: *const c_void;
    static kSecCodeInfoIdentifier: *const c_void;
    static kSecCodeInfoTeamIdentifier: *const c_void;
    static kSecCodeInfoCertificates: *const c_void;
    fn SecCodeCopyGuestWithAttributes(
        host: *const c_void,
        attributes: *const c_void,
        flags: u32,
        guest: *mut *const c_void,
    ) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        info: *mut *const c_void,
    ) -> i32;
    fn SecCodeCheckValidity(code: *const c_void, flags: u32, requirement: *const c_void) -> i32;
    fn SecCertificateCopySubjectSummary(certificate: *const c_void) -> *mut NSString;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(object: *const c_void);
}

/// `kSecCSSigningInformation`.
const SIGNING_INFORMATION: u32 = 1 << 1;

/// A running process's signature, in words: who signed it, its identifier
/// and team, and whether it checks out.
pub fn signing_of(pid: u32) -> String {
    // SAFETY: the Security framework's documented calls; every object it
    // hands over is released once, below, and the constants are CFStrings,
    // toll-free bridged to NSString.
    unsafe {
        let key = &*(kSecGuestAttributePid as *const NSString);
        let number = NSNumber::new_i32(pid as i32);
        let attributes: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&[key], &[&*number as &AnyObject]);
        let mut code: *const c_void = std::ptr::null();
        let status = SecCodeCopyGuestWithAttributes(
            std::ptr::null(),
            Retained::as_ptr(&attributes).cast(),
            0,
            &mut code,
        );
        if status != 0 || code.is_null() {
            return "its signature could not be read".to_string();
        }
        let valid = SecCodeCheckValidity(code, 0, std::ptr::null()) == 0;
        let mut info: *const c_void = std::ptr::null();
        let status = SecCodeCopySigningInformation(code, SIGNING_INFORMATION, &mut info);
        let said = if status != 0 || info.is_null() {
            "its signature could not be read".to_string()
        } else {
            let info_dict = &*(info as *const NSDictionary<NSString, AnyObject>);
            let field = |key: *const c_void| -> Option<String> {
                info_dict
                    .objectForKey(&*(key as *const NSString))
                    .and_then(|v| v.downcast::<NSString>().ok())
                    .map(|s| s.to_string())
            };
            let identifier = field(kSecCodeInfoIdentifier);
            let team = field(kSecCodeInfoTeamIdentifier);
            let signer = info_dict
                .objectForKey(&*(kSecCodeInfoCertificates as *const NSString))
                .and_then(|v| v.downcast::<NSArray>().ok())
                .and_then(|certificates| certificates.firstObject())
                .and_then(|leaf| {
                    let summary = SecCertificateCopySubjectSummary(Retained::as_ptr(&leaf).cast());
                    Retained::from_raw(summary).map(|s| s.to_string())
                });
            CFRelease(info);
            match (signer, identifier) {
                (Some(signer), identifier) => format!(
                    "signed by {signer}{}{}",
                    team.map(|t| format!(", team {t}")).unwrap_or_default(),
                    identifier.map(|i| format!(", as {i}")).unwrap_or_default()
                ),
                (None, Some(identifier)) => {
                    format!("signed ad hoc, as {identifier}, by no one Apple vouches for")
                }
                (None, None) => "not signed".to_string(),
            }
        };
        CFRelease(code);
        if valid {
            said
        } else {
            format!("{said}; its signature does not check out")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_and_its_parents_are_found() {
        let me = std::process::id();
        let chain = chain(me);
        assert_eq!(chain.first().map(|p| p.pid), Some(me));
        assert!(!chain[0].path.is_empty(), "{chain:?}");
        assert!(chain.iter().all(|p| p.pid > 1));
        // The same process is the same process: its start time holds still.
        assert_eq!(super::chain(me)[0], chain[0]);
        assert!(!signing_of(me).is_empty());
        assert_eq!(signing_of(u32::MAX >> 1), "its signature could not be read");
    }

    #[test]
    fn a_connected_peer_is_this_process_and_this_user() {
        let (a, b) = UnixStream::pair().unwrap();
        assert_eq!(pid_of(&a).unwrap(), std::process::id());
        assert_eq!(uid_of(&b).unwrap(), unsafe { libc::geteuid() });
    }
}
