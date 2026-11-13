//! The `--agy-broker` exchange folder: one job folder per turn, holding the
//! watcher's `request.json` and agy's `reply.json`.
//!
//! A Low-integrity agy sandbox can write here, so every access goes through
//! handles, never a path: the folders from the volume root to the exchange
//! folder are opened one by one, each relative to its parent, refusing any
//! reparse point, and held without delete sharing for the watcher's life.
//! The held handles also have no delete access, so agy's sandboxed shell
//! can open the folders of its `--add-dir` path to change their DACL.
//! Job folders and files are then created, opened, listed and deleted
//! relative to those handles.
//!
//! When agy's shell starts, it adds an entry that gives the sandbox SID
//! traverse and synchronize to each folder of the job folder's path where
//! the SID lacks that access. On the exchange folder that entry, and only
//! that one, is accepted beside the contract below.
//!
//! On Windows the exchange folder gets the descriptor that the design calls
//! the "descendant access contract": owner the current user; a protected
//! DACL of exactly user Full control, SYSTEM Full control and the sandbox
//! SID Modify, each object and container inherit; and a Low mandatory label
//! with no-write-up, object and container inherit. Everything below the
//! folder gets its access only by inheritance. On Unix the folder is mode
//! 0700 and has no label.
use std::io;
use std::path::{Path, PathBuf};

pub use imp::Descriptor;

/// The most entries removed from one job folder, and the deepest folder.
pub const CLEANUP_MAX_ENTRIES: usize = 1000;
pub const CLEANUP_MAX_DEPTH: usize = 8;

/// One entry in a folder listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    /// A reparse point (Windows) or symbolic link (Unix).
    pub link: bool,
}

/// What a capped read of a file in a job folder found.
#[derive(Debug, PartialEq, Eq)]
pub enum FileRead {
    Missing,
    Bytes(Vec<u8>),
    /// More than the cap; nothing past the cap was read.
    TooBig,
    /// Not a regular file with one link, a reparse point, or still open for
    /// writing. The text says which.
    Refused(String),
}

impl FileRead {
    /// True for a file that another process still has open for writing:
    /// read it again later.
    pub fn is_busy(&self) -> bool {
        matches!(self, FileRead::Refused(why) if why.ends_with(STILL_WRITING))
    }
}

/// The end of the `Refused` text for a file still open for writing.
const STILL_WRITING: &str = "is still open for writing";

/// The pinned exchange folder.
pub struct Exchange {
    path: PathBuf,
    dir: imp::Dir,
    /// Every folder above `dir`, held open so none can be renamed
    /// (Windows) and so the trust rule checks the folders in use (Unix).
    pins: Vec<imp::Dir>,
}

/// A pinned job folder inside the exchange folder.
pub struct JobDir {
    name: String,
    path: PathBuf,
    dir: imp::Dir,
}

impl Exchange {
    /// The location `path` names: the part that exists, resolved once from
    /// its folder handle (Unix: symlinks; Windows: 8.3 names, case and
    /// `subst` drives, with junctions refused), and the missing rest
    /// appended. Check this location, then give it to `open_or_create` and
    /// use it in every path given to agy.
    pub fn resolve(path: &Path) -> io::Result<PathBuf> {
        imp::resolve(path)
    }

    /// Pin `path` (a location from `resolve`) and check its descriptor, or
    /// create it with the contract above when it does not exist. `sid` is
    /// the sandbox SID (Windows). Its parent folders are created first if
    /// needed. The walk follows no link.
    pub fn open_or_create(path: &Path, sid: Option<&str>) -> io::Result<Exchange> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("the exchange folder has no parent"))?;
        let leaf = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("the exchange folder name is not valid"))?;
        // Missing parents are made in the same handle walk that checks each
        // component, so nothing is made through a junction.
        let pins = imp::pin(parent, true)?;
        let parent_dir = pins.last().expect("at least the volume root");
        let dir = match imp::open_dir(parent_dir, leaf)? {
            Some(dir) => {
                imp::check_exchange(&dir, sid)?;
                dir
            }
            None => imp::create_exchange(parent_dir, leaf, sid)?,
        };
        let ex = Exchange {
            path: path.to_owned(),
            dir,
            pins,
        };
        imp::check_trust(&ex.chain(None), path)?;
        Ok(ex)
    }

    /// The pinned folders from the volume root down to `job` (or to the
    /// exchange folder).
    fn chain<'a>(&'a self, job: Option<&'a JobDir>) -> Vec<&'a imp::Dir> {
        self.pins
            .iter()
            .chain([&self.dir])
            .chain(job.map(|j| &j.dir))
            .collect()
    }

    /// Unix: fail unless every folder from `/` down to `job`, checked by
    /// its pinned descriptor now, still meets the trust rule (owned by root
    /// or this user, and not writable by group or others unless sticky
    /// with a folder of root or this user in it). Then nobody outside this
    /// user and root can rename a folder of the path or plant another in
    /// its place before agy opens it. Windows: always Ok; the pins deny
    /// delete sharing, so nothing on the path can be renamed.
    pub fn check_trust(&self, job: &JobDir) -> io::Result<()> {
        imp::check_trust(&self.chain(Some(job)), &job.path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entries of the exchange folder, through its handle.
    pub fn entries(&self) -> io::Result<Vec<Entry>> {
        imp::list(&self.dir)
    }

    /// Create the job folder `name`, failing if it exists, and check that it
    /// inherited the Low label.
    pub fn create_job(&self, name: &str) -> io::Result<JobDir> {
        let dir = imp::create_job(&self.dir, name)?;
        Ok(JobDir {
            name: name.to_owned(),
            path: self.path.join(name),
            dir,
        })
    }

    /// Open the job folder `name`. `None` when it does not exist. A reparse
    /// point or a file is refused.
    pub fn open_job(&self, name: &str) -> io::Result<Option<JobDir>> {
        Ok(imp::open_dir(&self.dir, name)?.map(|dir| JobDir {
            name: name.to_owned(),
            path: self.path.join(name),
            dir,
        }))
    }

    /// Delete the entry `name` (a link is deleted, never followed), with
    /// the job folder limits.
    pub fn remove_entry(&self, name: &str) -> io::Result<()> {
        let mut budget = CLEANUP_MAX_ENTRIES;
        imp::remove_child(&self.dir, name, 0, &mut budget)
    }

    /// Delete everything in `job`, then `job` itself, by handle.
    pub fn remove_job(&self, job: JobDir) -> io::Result<()> {
        let mut budget = CLEANUP_MAX_ENTRIES;
        imp::remove_contents(&job.dir, 0, &mut budget)?;
        imp::remove_dir(&self.dir, &job.name, job.dir)
    }
}

impl JobDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Fail unless the folder's path, looked up by name now, still leads
    /// to the pinned folder: the path agy is given must be this folder.
    pub fn check_path(&self) -> io::Result<()> {
        if imp::same_folder(&self.dir, &self.path)? {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} now leads to another folder",
                self.path.display()
            )))
        }
    }

    /// Create the file `name` (failing if it exists) with `bytes`, flushed.
    pub fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        imp::write_new(&self.dir, name, bytes)
    }

    /// Read the file `name`, at most `cap` bytes.
    pub fn read_capped(&self, name: &str, cap: usize) -> FileRead {
        imp::read_capped(&self.dir, name, cap)
    }

    /// The folder's descriptor, for checks and tests.
    pub fn descriptor(&self) -> io::Result<Descriptor> {
        imp::descriptor(&self.dir)
    }
}

#[cfg(windows)]
pub(crate) mod imp {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::ptr::{null, null_mut};

    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
        HANDLE, HLOCAL, LocalFree, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
        STATUS_FILE_IS_A_DIRECTORY, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_NOT_FOUND,
        STATUS_SHARING_VIOLATION, UNICODE_STRING,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT,
        SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx, AddMandatoryAce,
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
        LABEL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR,
        SetSecurityDescriptorControl, SetSecurityDescriptorDacl, SetSecurityDescriptorOwner,
        TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED,
        GetFileInformationByHandle, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
        FILE_DISPOSITION_INFO_EX, FILE_EXECUTE, FILE_FULL_DIR_INFO, FILE_GENERIC_EXECUTE,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
        FILE_TRAVERSE, FILE_WRITE_DATA, FileAttributeTagInfo, FileDispositionInfo,
        FileDispositionInfoEx, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
        FileStandardInfo, GetFileInformationByHandleEx, READ_CONTROL, SYNCHRONIZE,
        SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{CLEANUP_MAX_DEPTH, Entry, FileRead, STILL_WRITING};

    pub type Dir = OwnedHandle;

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
    const SYSTEM_MANDATORY_LABEL_NO_WRITE_UP: u32 = 1;
    const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
    const INHERIT: u8 = (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8;
    pub const SYSTEM_SID: &str = "S-1-5-18";
    pub const LOW_LABEL_SID: &str = "S-1-16-4096";
    /// Modify: read, write, execute and delete.
    pub const MODIFY: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE;

    /// One access-allowed or label entry, with its SID as a string.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Ace {
        pub kind: u8,
        pub flags: u8,
        pub mask: u32,
        pub sid: String,
    }

    /// The parts of a security descriptor that the contract covers.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Descriptor {
        pub owner: String,
        pub protected: bool,
        /// `None` for a null DACL (everyone has full access).
        pub dacl: Option<Vec<Ace>>,
        pub label: Vec<Ace>,
    }

    impl Descriptor {
        /// The exchange folder's descriptor for `user` and `sandbox`.
        pub fn contract(user: &str, sandbox: &str) -> Descriptor {
            let allow = |mask, sid: &str| Ace {
                kind: ACCESS_ALLOWED_ACE_TYPE,
                flags: INHERIT,
                mask,
                sid: sid.to_owned(),
            };
            Descriptor {
                owner: user.to_owned(),
                protected: true,
                dacl: Some(vec![
                    allow(FILE_ALL_ACCESS, user),
                    allow(FILE_ALL_ACCESS, SYSTEM_SID),
                    allow(MODIFY, sandbox),
                ]),
                label: vec![low_label(INHERIT)],
            }
        }

        /// Why `self` is not `want`, compared as sets: the order of the
        /// entries does not matter. `None` when they match.
        pub fn mismatch(&self, want: &Descriptor) -> Option<String> {
            if self.owner != want.owner {
                return Some(format!("owner is {}, not {}", self.owner, want.owner));
            }
            if self.protected != want.protected {
                return Some("the DACL is not protected from inheritance".into());
            }
            let sorted = |v: &Option<Vec<Ace>>| {
                v.clone().map(|mut v| {
                    v.sort();
                    v
                })
            };
            if sorted(&self.dacl) != sorted(&want.dacl) {
                return Some(format!("the DACL is {:?}, not {:?}", self.dacl, want.dacl));
            }
            let mut have = self.label.clone();
            have.sort();
            if have != want.label {
                return Some(format!(
                    "the label is {:?}, not {:?}",
                    self.label, want.label
                ));
            }
            None
        }

        /// True when the label is Low with no-write-up.
        pub fn is_low(&self) -> bool {
            self.label.len() == 1
                && self.label[0].kind == SYSTEM_MANDATORY_LABEL_ACE_TYPE
                && self.label[0].sid == LOW_LABEL_SID
                && self.label[0].mask & SYSTEM_MANDATORY_LABEL_NO_WRITE_UP != 0
        }
    }

    pub fn low_label(flags: u8) -> Ace {
        Ace {
            kind: SYSTEM_MANDATORY_LABEL_ACE_TYPE,
            flags,
            mask: SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            sid: LOW_LABEL_SID.to_owned(),
        }
    }

    /// An access-allowed entry, for tests that build other descriptors.
    #[cfg(test)]
    pub fn allow(mask: u32, flags: u8, sid: &str) -> Ace {
        Ace {
            kind: ACCESS_ALLOWED_ACE_TYPE,
            flags,
            mask,
            sid: sid.to_owned(),
        }
    }

    #[cfg(test)]
    pub const INHERIT_FLAGS: u8 = INHERIT;
    #[cfg(test)]
    pub const INHERITED_FLAG: u8 = 0x10; // INHERITED_ACE

    fn nt_error(status: NTSTATUS) -> io::Error {
        // SAFETY: a pure conversion with no pointers.
        io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn raw(h: &OwnedHandle) -> HANDLE {
        h.as_raw_handle() as HANDLE
    }

    /// `NtCreateFile` of `name`, relative to `parent` when given.
    #[allow(clippy::too_many_arguments)]
    fn nt_create(
        parent: Option<&OwnedHandle>,
        name: &[u16],
        access: u32,
        attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        sd: *const SECURITY_DESCRIPTOR,
    ) -> Result<OwnedHandle, NTSTATUS> {
        let bytes = u16::try_from(name.len() * 2).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
        let us = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: name.as_ptr() as *mut u16,
        };
        let oa = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.map_or(null_mut(), raw),
            ObjectName: &us,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: sd,
            SecurityQualityOfService: null(),
        };
        let mut h: HANDLE = null_mut();
        // SAFETY: an all-zero IO_STATUS_BLOCK is valid output space.
        let mut iosb: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer refers to a live local for the whole call.
        let status = unsafe {
            NtCreateFile(
                &mut h,
                access | SYNCHRONIZE,
                &oa,
                &mut iosb,
                null(),
                attributes,
                share,
                disposition,
                options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                null(),
                0,
            )
        };
        if status < 0 {
            return Err(status);
        }
        // SAFETY: NtCreateFile succeeded, so `h` is a new handle we own.
        Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
    }

    /// One path component: not empty, no separators, not `.` or `..`.
    fn component(name: &str) -> io::Result<Vec<u16>> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['\\', '/', ':']) {
            return Err(io::Error::other(format!("invalid name {name:?}")));
        }
        Ok(wide(name))
    }

    fn attributes(h: &OwnedHandle) -> io::Result<u32> {
        let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: `info` is the right size for this class.
        if unsafe {
            GetFileInformationByHandleEx(
                raw(h),
                FileAttributeTagInfo,
                (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(info.FileAttributes)
    }

    fn refuse_reparse(h: &OwnedHandle, what: &str) -> io::Result<()> {
        if attributes(h)? & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(format!(
                "{what} is a reparse point (junction or link); refused"
            )));
        }
        Ok(())
    }

    const PIN_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

    /// Open each folder of `path`, from the volume root, relative to the one
    /// before, refusing reparse points. With `create`, a missing folder is
    /// made (it is opened, not followed, if it is a reparse point).
    pub fn pin(path: &Path, create: bool) -> io::Result<Vec<OwnedHandle>> {
        let mut parts = path.components();
        let drive = match parts.next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d as char,
                _ => {
                    return Err(io::Error::other(
                        "the exchange folder must be on a drive letter path",
                    ));
                }
            },
            _ => {
                return Err(io::Error::other(
                    "the exchange folder must be an absolute path",
                ));
            }
        };
        if parts.next() != Some(Component::RootDir) {
            return Err(io::Error::other(
                "the exchange folder must be an absolute path",
            ));
        }
        let root = wide(&format!("\\??\\{drive}:\\"));
        let access = FILE_TRAVERSE | FILE_READ_ATTRIBUTES;
        let mut pins = vec![
            nt_create(
                None,
                &root,
                access,
                0,
                PIN_SHARE,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
                null(),
            )
            .map_err(nt_error)?,
        ];
        for part in parts {
            let Component::Normal(name) = part else {
                return Err(io::Error::other("the exchange path holds . or .."));
            };
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("the exchange path is not valid Unicode"))?;
            let h = nt_create(
                pins.last(),
                &component(name)?,
                access,
                if create { FILE_ATTRIBUTE_DIRECTORY } else { 0 },
                PIN_SHARE,
                if create { FILE_OPEN_IF } else { FILE_OPEN },
                FILE_DIRECTORY_FILE,
                null(),
            )
            .map_err(|s| {
                io::Error::new(nt_error(s).kind(), format!("open {name}: {}", nt_error(s)))
            })?;
            refuse_reparse(&h, name)?;
            pins.push(h);
        }
        Ok(pins)
    }

    /// `path` with its existing part replaced by the final path of its
    /// pinned handle, and the missing rest appended.
    pub fn resolve(path: &Path) -> io::Result<PathBuf> {
        let mut base = path;
        let mut missing = Vec::new();
        loop {
            match pin(base, false) {
                Ok(pins) => {
                    let mut real = final_path(pins.last().expect("the volume root"))?;
                    real.extend(missing.iter().rev());
                    return Ok(real);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let (Some(parent), Some(name)) = (base.parent(), base.file_name()) else {
                        return Err(e);
                    };
                    missing.push(name);
                    base = parent;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// The normalised drive-letter path of the folder `h`.
    fn final_path(h: &OwnedHandle) -> io::Result<PathBuf> {
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: `buf` holds `buf.len()` UTF-16 units.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    raw(h),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if n == 0 {
                return Err(io::Error::last_os_error());
            }
            if n < buf.len() {
                buf.truncate(n);
                break;
            }
            // Too small: `n` is the size needed, with the NUL.
            buf.resize(n, 0);
        }
        let text = String::from_utf16(&buf)
            .map_err(|_| io::Error::other("the exchange path is not valid Unicode"))?;
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        if !matches!(text.as_bytes(), [d, b':', b'\\', ..] if d.is_ascii_alphabetic()) {
            return Err(io::Error::other(format!(
                "the exchange folder must be on a drive letter path, not {text}"
            )));
        }
        Ok(PathBuf::from(text))
    }

    fn file_id(h: &OwnedHandle) -> io::Result<(u32, u32, u32)> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `info` is valid output space.
        if unsafe { GetFileInformationByHandle(raw(h), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }

    /// Nothing to check: the pinned handles deny delete sharing, so no
    /// folder of the path can be renamed while the exchange is open.
    pub fn check_trust(_dirs: &[&OwnedHandle], _path: &Path) -> io::Result<()> {
        Ok(())
    }

    /// True when `path`, opened by name now, is the folder (or file) `h`.
    pub fn same_folder(h: &OwnedHandle, path: &Path) -> io::Result<bool> {
        use std::os::windows::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        Ok(file_id(&OwnedHandle::from(f))? == file_id(h)?)
    }

    /// The access of the held exchange and job folder handles. No DELETE:
    /// when agy's sandboxed shell starts, it opens each folder of the
    /// `--add-dir` path without delete sharing to add an entry for the
    /// sandbox SID, and a held handle with DELETE access makes that fail
    /// with a sharing violation. The handles still deny delete sharing,
    /// so the folders cannot be renamed; `remove_dir` opens a folder with
    /// DELETE only to delete it.
    const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY
        | FILE_TRAVERSE
        | FILE_ADD_FILE
        | FILE_ADD_SUBDIRECTORY
        | FILE_READ_ATTRIBUTES
        | READ_CONTROL;

    /// Open the folder `name` in `parent`. `None` when it does not exist.
    pub fn open_dir(parent: &OwnedHandle, name: &str) -> io::Result<Option<OwnedHandle>> {
        let access = DIR_ACCESS;
        match nt_create(
            Some(parent),
            &component(name)?,
            access,
            0,
            PIN_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            null(),
        ) {
            Ok(h) => {
                refuse_reparse(&h, name)?;
                Ok(Some(h))
            }
            Err(STATUS_OBJECT_NAME_NOT_FOUND) => Ok(None),
            Err(STATUS_NOT_A_DIRECTORY) => Err(io::Error::other(format!("{name} is not a folder"))),
            Err(s) => Err(nt_error(s)),
        }
    }

    struct LocalSid(PSID);
    impl Drop for LocalSid {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSidToSidW.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }

    fn parse_sid(sid: &str) -> io::Result<LocalSid> {
        let w: Vec<u16> = wide(sid).into_iter().chain([0]).collect();
        let mut p: PSID = null_mut();
        // SAFETY: `w` is NUL-terminated; `p` receives a LocalAlloc'd SID.
        if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
            return Err(io::Error::other(format!("{sid:?} is not a valid SID")));
        }
        Ok(LocalSid(p))
    }

    /// True when `sid` parses as a SID.
    pub fn valid_sid(sid: &str) -> bool {
        parse_sid(sid).is_ok()
    }

    fn sid_string(p: PSID) -> io::Result<String> {
        let mut s: *mut u16 = null_mut();
        // SAFETY: `p` is a valid SID from a descriptor or token.
        if unsafe { ConvertSidToStringSidW(p, &mut s) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `s` is a NUL-terminated string that we free below.
        let out = unsafe {
            let len = (0..).take_while(|&i| *s.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(s, len))
        };
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(s as HLOCAL) };
        Ok(out)
    }

    /// The current user's SID, from the process token.
    pub fn current_user() -> io::Result<String> {
        let buf = token_info(TokenUser)?;
        // SAFETY: GetTokenInformation wrote a TOKEN_USER at the start.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        sid_string(user.User.Sid)
    }

    /// The owner the process token gives new objects: the user, or
    /// BUILTIN\Administrators in an elevated token.
    pub fn token_owner() -> io::Result<String> {
        let buf = token_info(TokenOwner)?;
        // SAFETY: GetTokenInformation wrote a TOKEN_OWNER at the start.
        let owner = unsafe { &*(buf.as_ptr() as *const TOKEN_OWNER) };
        sid_string(owner.Owner)
    }

    /// The process token's `class` information, in a buffer that also
    /// holds the SIDs it points to.
    fn token_info(class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
        let mut token: HANDLE = null_mut();
        // SAFETY: the pseudo handle needs no closing; `token` is closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken succeeded, so we own `token`.
        let token = unsafe { OwnedHandle::from_raw_handle(token as RawHandle) };
        let mut buf = vec![0u64; 128];
        let mut len = 0u32;
        // SAFETY: `buf` is 1024 bytes, aligned for the token structures.
        if unsafe {
            GetTokenInformation(
                raw(&token),
                class,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }

    /// An ACL holding `aces`, in a buffer that must outlive its use.
    fn build_acl(aces: &[Ace]) -> io::Result<Vec<u64>> {
        let mut buf = vec![0u64; 256];
        let acl = buf.as_mut_ptr() as *mut ACL;
        // SAFETY: `buf` is 2048 bytes, aligned for ACL.
        if unsafe { InitializeAcl(acl, (buf.len() * 8) as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        for ace in aces {
            let sid = parse_sid(&ace.sid)?;
            // SAFETY: `acl` was initialised above; `sid` is valid.
            let ok = unsafe {
                if ace.kind == SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                    AddMandatoryAce(acl, ACL_REVISION, ace.flags as u32, ace.mask, sid.0)
                } else {
                    AddAccessAllowedAceEx(acl, ACL_REVISION, ace.flags as u32, ace.mask, sid.0)
                }
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(buf)
    }

    fn read_aces(acl: *const ACL) -> io::Result<Vec<Ace>> {
        let mut out = Vec::new();
        // SAFETY: `acl` comes from GetSecurityInfo and is valid.
        let count = unsafe { (*acl).AceCount };
        for i in 0..count as u32 {
            let mut ace: *mut core::ffi::c_void = null_mut();
            // SAFETY: `i` is below the ACE count.
            if unsafe { GetAce(acl, i, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with a header; both the
            // access-allowed and the label ACE have the mask and the SID at
            // the same offsets.
            let (header, mask, sid) = unsafe {
                let header = *(ace as *const ACE_HEADER);
                let a = ace as *const ACCESS_ALLOWED_ACE;
                (header, (*a).Mask, &(*a).SidStart as *const u32 as PSID)
            };
            let sid = if header.AceType == ACCESS_ALLOWED_ACE_TYPE
                || header.AceType == SYSTEM_MANDATORY_LABEL_ACE_TYPE
            {
                sid_string(sid)?
            } else {
                String::new()
            };
            out.push(Ace {
                kind: header.AceType,
                flags: header.AceFlags,
                mask,
                sid,
            });
        }
        Ok(out)
    }

    /// The owner, DACL and label of the object behind `h`.
    pub fn descriptor(h: &OwnedHandle) -> io::Result<Descriptor> {
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut sacl: *mut ACL = null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: the out pointers point into `sd`, freed below.
        let err = unsafe {
            GetSecurityInfo(
                raw(h),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                &mut sacl,
                &mut sd,
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        let result = (|| {
            let mut control = 0u16;
            let mut revision = 0u32;
            // SAFETY: `sd` is the descriptor GetSecurityInfo returned.
            if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Descriptor {
                owner: sid_string(owner)?,
                protected: control & SE_DACL_PROTECTED != 0,
                dacl: if dacl.is_null() {
                    None
                } else {
                    Some(read_aces(dacl)?)
                },
                label: if sacl.is_null() {
                    Vec::new()
                } else {
                    read_aces(sacl)?
                },
            })
        })();
        // SAFETY: allocated by GetSecurityInfo.
        unsafe { LocalFree(sd as HLOCAL) };
        result
    }

    /// Access bits that let a holder change an object or what it holds.
    const WRITE_BITS: u32 = 0x2 // FILE_WRITE_DATA, FILE_ADD_FILE
        | 0x4 // FILE_APPEND_DATA, FILE_ADD_SUBDIRECTORY
        | 0x10 // FILE_WRITE_EA
        | 0x40 // FILE_DELETE_CHILD
        | 0x100 // FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | 0x1000_0000 // GENERIC_ALL
        | 0x4000_0000; // GENERIC_WRITE
    /// The access-denied ACE types; any other DACL entry may allow.
    const DENY_TYPES: [u8; 4] = [1, 6, 0xA, 0xC];
    /// Medium integrity (`S-1-16-8192`).
    const MEDIUM_RID: u32 = 8192;

    /// The restricted-token SIDs, RESTRICTED and WRITE RESTRICTED: a grant
    /// to either lets a restricted (sandboxed) token pass its second check.
    const RESTRICTED_SIDS: [&str; 2] = ["S-1-5-12", "S-1-5-33"];

    /// Why the object with descriptor `d` is not trusted for session mode,
    /// or `None`. Refused: a null DACL; a label below Medium, or a Medium or
    /// higher label without no-write-up; an allow entry with write access
    /// for any app-container or capability SID (`S-1-15-...`, listed in
    /// `sids` or not), the Low label SID, a restricted-token SID or one of
    /// `sids`; an allow entry of another type than the plain one with write
    /// access. Inherit-only entries count too: they reach the folders
    /// below. Ordinary principals (the owner, users, administrators,
    /// SYSTEM) pass: named sandbox groups cannot be told apart from them.
    pub fn untrusted(d: &Descriptor, sids: &[String]) -> Option<String> {
        let Some(dacl) = &d.dacl else {
            return Some("it has a null DACL, so anyone can write it".into());
        };
        for ace in &d.label {
            if ace.kind != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                continue;
            }
            let rid = ace
                .sid
                .strip_prefix("S-1-16-")
                .and_then(|r| r.parse::<u32>().ok());
            match rid {
                Some(r)
                    if r >= MEDIUM_RID && ace.mask & SYSTEM_MANDATORY_LABEL_NO_WRITE_UP != 0 => {}
                Some(r) if r >= MEDIUM_RID => {
                    return Some("its integrity label has no no-write-up".into());
                }
                _ => return Some("it has an integrity label below Medium".into()),
            }
        }
        for ace in dacl {
            if DENY_TYPES.contains(&ace.kind) || ace.mask & WRITE_BITS == 0 {
                continue;
            }
            if ace.kind != ACCESS_ALLOWED_ACE_TYPE {
                return Some(format!(
                    "it has an allow entry of type {} with write access",
                    ace.kind
                ));
            }
            let sandbox = ace.sid.starts_with("S-1-15-")
                || ace.sid == LOW_LABEL_SID
                || RESTRICTED_SIDS.contains(&ace.sid.as_str())
                || sids.iter().any(|s| s.eq_ignore_ascii_case(&ace.sid));
            if sandbox {
                return Some(format!("it gives write access to {}", ace.sid));
            }
        }
        None
    }

    /// Open each part of `path` from the volume root, relative to the one
    /// before, refusing reparse points, and check each with `untrusted`.
    /// The last part is a file when `file` is true, else a folder; with
    /// `read`, at most that many bytes of the file are read through its
    /// checked handle. Returns the folder handles, held without delete
    /// sharing, and the bytes.
    pub fn pin_trusted(
        path: &Path,
        file: bool,
        read: Option<usize>,
        sids: &[String],
    ) -> io::Result<(Vec<OwnedHandle>, Option<Vec<u8>>)> {
        walk_trusted(path, file, read, sids, false)
    }

    /// `pin_trusted` for the file `path`, whose checked handle is held too,
    /// without write or delete sharing: while it is held, the file cannot
    /// be changed, renamed, replaced or deleted. Returns the folder handles
    /// and the file's handle.
    pub fn pin_file(path: &Path, sids: &[String]) -> io::Result<(Vec<OwnedHandle>, OwnedHandle)> {
        let (mut pins, _) = walk_trusted(path, true, None, sids, true)?;
        let file = pins
            .pop()
            .ok_or_else(|| io::Error::other("no file was opened"))?;
        Ok((pins, file))
    }

    fn walk_trusted(
        path: &Path,
        file: bool,
        read: Option<usize>,
        sids: &[String],
        keep: bool,
    ) -> io::Result<(Vec<OwnedHandle>, Option<Vec<u8>>)> {
        let parts: Vec<Component> = path.components().collect();
        let drive = match parts.first() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d as char,
                _ => return Err(io::Error::other("it is not on a drive letter path")),
            },
            _ => return Err(io::Error::other("it is not an absolute path")),
        };
        if parts.get(1) != Some(&Component::RootDir) || parts.len() < 3 {
            return Err(io::Error::other("it is not an absolute path below a drive"));
        }
        let check = |h: &OwnedHandle, what: &str| -> io::Result<()> {
            refuse_reparse(h, what)?;
            let d = descriptor(h)
                .map_err(|e| io::Error::other(format!("read the security of {what}: {e}")))?;
            match untrusted(&d, sids) {
                Some(why) => Err(io::Error::other(format!("{what}: {why}"))),
                None => Ok(()),
            }
        };
        let access = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL;
        let volume = nt_create(
            None,
            &wide(&format!("\\??\\{drive}:\\")),
            access,
            0,
            PIN_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            null(),
        )
        .map_err(nt_error)?;
        check(&volume, &format!("{drive}:\\"))?;
        let mut pins = vec![volume];
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate().skip(2) {
            let Component::Normal(name) = part else {
                return Err(io::Error::other("the path holds . or .."));
            };
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("the path is not valid Unicode"))?;
            let leaf = i == last && file;
            let (access, share, options) = if leaf {
                // A held file needs a data access: a handle with none does
                // not take part in sharing, so it would block nothing.
                let data = match (read, keep) {
                    (Some(_), _) => FILE_READ_DATA,
                    (None, true) => FILE_EXECUTE,
                    (None, false) => 0,
                };
                (
                    data | FILE_READ_ATTRIBUTES | READ_CONTROL,
                    FILE_SHARE_READ,
                    FILE_NON_DIRECTORY_FILE,
                )
            } else {
                (access, PIN_SHARE, FILE_DIRECTORY_FILE)
            };
            let h = nt_create(
                pins.last(),
                &component(name)?,
                access,
                0,
                share,
                FILE_OPEN,
                options,
                null(),
            )
            .map_err(|s| {
                io::Error::new(nt_error(s).kind(), format!("open {name}: {}", nt_error(s)))
            })?;
            check(&h, name)?;
            if leaf {
                let bytes = match read {
                    Some(cap) => {
                        let mut bytes = Vec::new();
                        File::from(h).take(cap as u64 + 1).read_to_end(&mut bytes)?;
                        if bytes.len() > cap {
                            return Err(io::Error::other(format!("{name} is over {cap} bytes")));
                        }
                        Some(bytes)
                    }
                    None => {
                        if keep {
                            pins.push(h);
                        }
                        None
                    }
                };
                return Ok((pins, bytes));
            }
            pins.push(h);
        }
        Ok((pins, None))
    }

    /// Set the DACL (and its protection) and the owner of `h`. For tests
    /// that build a folder with another descriptor.
    #[cfg(test)]
    pub fn set_dacl(h: &OwnedHandle, aces: &[Ace], protected: bool) -> io::Result<()> {
        use windows_sys::Win32::Security::{
            PROTECTED_DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
        };
        let acl = build_acl(aces)?;
        let info = DACL_SECURITY_INFORMATION
            | if protected {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
        // SAFETY: `acl` lives for the call.
        let err = unsafe {
            SetSecurityInfo(
                raw(h),
                SE_FILE_OBJECT,
                info,
                null_mut(),
                null_mut(),
                acl.as_ptr().cast(),
                null(),
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    }

    /// Set the Low label with object and container inherit on `h`.
    pub fn set_low_label(h: &OwnedHandle) -> io::Result<()> {
        let sacl = build_acl(&[low_label(INHERIT)])?;
        // SAFETY: `sacl` lives for the call.
        let err = unsafe {
            SetSecurityInfo(
                raw(h),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null(),
                sacl.as_ptr().cast(),
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    }

    fn sandbox(sid: Option<&str>) -> io::Result<&str> {
        let sid =
            sid.ok_or_else(|| io::Error::other("--agy-sandbox-sid is required on Windows"))?;
        parse_sid(sid)?;
        Ok(sid)
    }

    /// What agy's sandbox adds for its SID on each folder of an `--add-dir`
    /// path when its shell starts: traverse and synchronize.
    pub const TRAVERSE_GRANT: u32 = FILE_TRAVERSE | SYNCHRONIZE;

    /// True for an entry that agy's sandbox adds: allow, at most traverse
    /// and synchronize, for `sandbox`, and neither inheritable nor
    /// inherited. It gives the sandbox less than its Modify entry and
    /// reaches nothing below the folder.
    fn is_traverse_grant(ace: &Ace, sandbox: &str) -> bool {
        ace.kind == ACCESS_ALLOWED_ACE_TYPE
            && ace.flags == 0
            && ace.mask & !TRAVERSE_GRANT == 0
            && ace.sid == sandbox
    }

    pub fn check_exchange(h: &OwnedHandle, sid: Option<&str>) -> io::Result<()> {
        let sandbox = sandbox(sid)?;
        let mut want = Descriptor::contract(&current_user()?, sandbox);
        let mut have = descriptor(h)?;
        if let Some(dacl) = &mut have.dacl {
            dacl.retain(|a| !is_traverse_grant(a, sandbox));
        }
        // A folder made without an explicit owner gets the token's default
        // owner, which is BUILTIN\Administrators when elevated. That owner
        // is this process too, so it is accepted beside the user.
        if have.owner == token_owner()? {
            want.owner = have.owner.clone();
        }
        if let Some(why) = have.mismatch(&want) {
            return Err(io::Error::other(format!(
                "the exchange folder does not have the expected access: {why}; delete it and the watcher creates it again"
            )));
        }
        Ok(())
    }

    /// Create the exchange folder with the owner and protected DACL in the
    /// create call, then set the Low label and check the result. On any
    /// failure the new folder is deleted by its handle.
    pub fn create_exchange(
        parent: &OwnedHandle,
        name: &str,
        sid: Option<&str>,
    ) -> io::Result<OwnedHandle> {
        let user = current_user()?;
        let want = Descriptor::contract(&user, sandbox(sid)?);
        let acl = build_acl(want.dacl.as_deref().expect("contract has a DACL"))?;
        let owner = parse_sid(&user)?;
        let mut sd = SECURITY_DESCRIPTOR::default();
        let psd = &mut sd as *mut SECURITY_DESCRIPTOR as PSECURITY_DESCRIPTOR;
        // SAFETY: `sd`, `acl` and `owner` outlive the create call below.
        unsafe {
            if InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) == 0
                || SetSecurityDescriptorOwner(psd, owner.0, 0) == 0
                || SetSecurityDescriptorDacl(psd, 1, acl.as_ptr().cast(), 0) == 0
                || SetSecurityDescriptorControl(psd, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let h = nt_create(
            Some(parent),
            &component(name)?,
            DIR_ACCESS | WRITE_DAC | WRITE_OWNER,
            FILE_ATTRIBUTE_DIRECTORY,
            PIN_SHARE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
            &sd,
        )
        .map_err(nt_error)?;
        let checked = set_low_label(&h)
            .map_err(|e| {
                io::Error::other(format!(
                    "could not set the Low label on the exchange folder (needs WRITE_OWNER): {e}"
                ))
            })
            .and_then(|()| descriptor(&h))
            .and_then(|d| match d.mismatch(&want) {
                Some(why) => Err(io::Error::other(format!(
                    "the new exchange folder does not have the expected access: {why}"
                ))),
                None => Ok(()),
            });
        if let Err(e) = checked {
            let _ = remove_dir(parent, name, h);
            return Err(e);
        }
        Ok(h)
    }

    pub fn create_job(exchange: &OwnedHandle, name: &str) -> io::Result<OwnedHandle> {
        let h = nt_create(
            Some(exchange),
            &component(name)?,
            DIR_ACCESS,
            FILE_ATTRIBUTE_DIRECTORY,
            PIN_SHARE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
            null(),
        )
        .map_err(nt_error)?;
        match descriptor(&h) {
            Ok(d) if d.is_low() => Ok(h),
            Ok(d) => {
                let _ = remove_dir(exchange, name, h);
                Err(io::Error::other(format!(
                    "job folder {name} did not inherit the Low label (label {:?})",
                    d.label
                )))
            }
            Err(e) => {
                let _ = remove_dir(exchange, name, h);
                Err(e)
            }
        }
    }

    pub fn write_new(dir: &OwnedHandle, name: &str, bytes: &[u8]) -> io::Result<()> {
        let h = nt_create(
            Some(dir),
            &component(name)?,
            FILE_WRITE_DATA | FILE_READ_ATTRIBUTES,
            FILE_ATTRIBUTE_NORMAL,
            0,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
            null(),
        )
        .map_err(nt_error)?;
        let mut f = File::from(h);
        f.write_all(bytes)?;
        f.sync_all()
    }

    pub fn read_capped(dir: &OwnedHandle, name: &str, cap: usize) -> FileRead {
        let Ok(w) = component(name) else {
            return FileRead::Refused(format!("invalid name {name:?}"));
        };
        let h = match nt_create(
            Some(dir),
            &w,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES,
            0,
            FILE_SHARE_READ,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            null(),
        ) {
            Ok(h) => h,
            Err(STATUS_OBJECT_NAME_NOT_FOUND) => return FileRead::Missing,
            Err(STATUS_SHARING_VIOLATION) => {
                return FileRead::Refused(format!("{name} {STILL_WRITING}"));
            }
            Err(STATUS_FILE_IS_A_DIRECTORY) => {
                return FileRead::Refused(format!("{name} is a folder"));
            }
            Err(s) => return FileRead::Refused(format!("open {name}: {}", nt_error(s))),
        };
        match attributes(&h) {
            Ok(a) if a & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                return FileRead::Refused(format!("{name} is a reparse point"));
            }
            Ok(_) => {}
            Err(e) => return FileRead::Refused(format!("read {name}: {e}")),
        }
        let mut info = FILE_STANDARD_INFO::default();
        // SAFETY: `info` is the right size for this class.
        if unsafe {
            GetFileInformationByHandleEx(
                raw(&h),
                FileStandardInfo,
                (&mut info as *mut FILE_STANDARD_INFO).cast(),
                size_of::<FILE_STANDARD_INFO>() as u32,
            )
        } == 0
        {
            return FileRead::Refused(format!("read {name}: {}", io::Error::last_os_error()));
        }
        if info.Directory || info.NumberOfLinks != 1 {
            return FileRead::Refused(format!("{name} is not a regular file with one link"));
        }
        let mut bytes = Vec::new();
        if let Err(e) = File::from(h).take(cap as u64 + 1).read_to_end(&mut bytes) {
            return FileRead::Refused(format!("read {name}: {e}"));
        }
        if bytes.len() > cap {
            return FileRead::TooBig;
        }
        FileRead::Bytes(bytes)
    }

    pub fn list(dir: &OwnedHandle) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        let mut buf = vec![0u64; 8192];
        let mut class = FileFullDirectoryRestartInfo;
        loop {
            // SAFETY: `buf` is 64 KiB, aligned for FILE_FULL_DIR_INFO.
            if unsafe {
                GetFileInformationByHandleEx(
                    raw(dir),
                    class,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                )
            } == 0
            {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    return Ok(out);
                }
                return Err(e);
            }
            class = FileFullDirectoryInfo;
            let mut offset = 0usize;
            loop {
                // SAFETY: the system wrote a chain of entries into `buf`;
                // each offset and name length come from it.
                let (next, attrs, name) = unsafe {
                    let info = (buf.as_ptr() as *const u8).add(offset) as *const FILE_FULL_DIR_INFO;
                    let len = (*info).FileNameLength as usize / 2;
                    let name = std::slice::from_raw_parts((*info).FileName.as_ptr(), len);
                    (
                        (*info).NextEntryOffset as usize,
                        (*info).FileAttributes,
                        OsString::from_wide(name),
                    )
                };
                let name = name.to_string_lossy().into_owned();
                if name != "." && name != ".." {
                    out.push(Entry {
                        name,
                        dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
                        link: attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                    });
                }
                if next == 0 {
                    break;
                }
                offset += next;
            }
        }
    }

    /// Mark `h` for deletion; the object goes when the last handle closes
    /// (at once with POSIX semantics).
    pub fn delete(h: &OwnedHandle) -> io::Result<()> {
        let ex = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        };
        // SAFETY: `ex` is the right size for this class.
        if unsafe {
            SetFileInformationByHandle(
                raw(h),
                FileDispositionInfoEx,
                (&ex as *const FILE_DISPOSITION_INFO_EX).cast(),
                size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        } != 0
        {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        let old = [
            ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
            ERROR_INVALID_FUNCTION,
        ];
        if !old.iter().any(|&c| e.raw_os_error() == Some(c as i32)) {
            return Err(e);
        }
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `info` is the right size for this class.
        if unsafe {
            SetFileInformationByHandle(
                raw(h),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Delete `name` in `dir`: a link itself, a file, or a folder and its
    /// contents, all relative to handles.
    pub fn remove_child(
        dir: &OwnedHandle,
        name: &str,
        depth: usize,
        budget: &mut usize,
    ) -> io::Result<()> {
        if *budget == 0 {
            return Err(io::Error::other(
                "more than 1000 entries to remove; refused",
            ));
        }
        *budget -= 1;
        let h = nt_create(
            Some(dir),
            &component(name)?,
            DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            0,
            null(),
        )
        .map_err(nt_error)?;
        let attrs = attributes(&h)?;
        if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 && attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            if depth + 1 >= CLEANUP_MAX_DEPTH {
                return Err(io::Error::other("folders nested more than 8 deep; refused"));
            }
            remove_contents(&h, depth + 1, budget)?;
        }
        delete(&h)
    }

    pub fn remove_contents(dir: &OwnedHandle, depth: usize, budget: &mut usize) -> io::Result<()> {
        for entry in list(dir)? {
            remove_child(dir, &entry.name, depth, budget)?;
        }
        Ok(())
    }

    /// Delete the empty folder `dir`, named `name` in `parent`. `dir` has
    /// no DELETE access (see `DIR_ACCESS`) and denies delete sharing, so it
    /// is closed first. The folder is then opened again by name with
    /// DELETE, still without delete sharing, and deleted only if it is the
    /// same folder (volume serial and file index).
    pub fn remove_dir(parent: &OwnedHandle, name: &str, dir: OwnedHandle) -> io::Result<()> {
        let id = file_id(&dir)?;
        drop(dir);
        let h = nt_create(
            Some(parent),
            &component(name)?,
            DELETE | FILE_READ_ATTRIBUTES,
            0,
            PIN_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            null(),
        )
        .map_err(nt_error)?;
        refuse_reparse(&h, name)?;
        if file_id(&h)? != id {
            return Err(io::Error::other(format!(
                "{name} is now another folder; not deleted"
            )));
        }
        delete(&h)
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Component, Path, PathBuf};

    use super::{CLEANUP_MAX_DEPTH, Entry, FileRead};

    pub type Dir = OwnedFd;

    /// On Unix the contract is the owner and mode 0700; there is no label.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Descriptor {
        pub uid: u32,
        pub mode: u32,
    }

    fn name_c(name: &str) -> io::Result<CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(io::Error::other(format!("invalid name {name:?}")));
        }
        CString::new(name).map_err(|_| io::Error::other("name holds NUL"))
    }

    fn open_at(
        parent: libc::c_int,
        name: &CStr,
        flags: libc::c_int,
        mode: libc::c_uint,
    ) -> io::Result<OwnedFd> {
        // SAFETY: `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a new descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn stat(fd: &OwnedFd) -> io::Result<libc::stat> {
        // SAFETY: an all-zero stat is valid output space.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open.
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st)
    }

    const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY;

    /// The permission bits of `st`. `st_mode` is u32 on Linux and u16 on
    /// macOS, so the cast is needed on one and a no-op on the other.
    #[allow(clippy::unnecessary_cast)]
    fn mode_bits(st: &libc::stat) -> u32 {
        st.st_mode as u32 & 0o7777
    }

    fn euid() -> u32 {
        // SAFETY: no arguments.
        unsafe { libc::geteuid() }
    }

    /// The trust rule, like OpenSSH's StrictModes: only root and this user
    /// may be able to rename or replace a folder of the exchange path. The
    /// folder `st` (at `at`) must be owned by root or this user and not
    /// writable by group or others. A sticky folder (such as `/tmp`) may
    /// be, when `below`, the owner of the folder in it on the path, is
    /// root or this user: the sticky bit stops others renaming that
    /// folder. `below` is `None` for the last folder of the path.
    pub fn check_trusted(st: &libc::stat, below: Option<u32>, at: &Path) -> io::Result<()> {
        let me = euid();
        let trusted = |uid: u32| uid == 0 || uid == me;
        let mode = mode_bits(st);
        let why = if !trusted(st.st_uid) {
            format!("is owned by uid {}", st.st_uid)
        } else if mode & 0o022 == 0 {
            return Ok(());
        } else {
            match below {
                Some(uid) if mode & 0o1000 != 0 && trusted(uid) => return Ok(()),
                Some(uid) if mode & 0o1000 != 0 => format!(
                    "is sticky and shared (mode {mode:o}), and the folder in it is owned by uid {uid}"
                ),
                _ => format!("is writable by group or others (mode {mode:o})"),
            }
        };
        Err(io::Error::other(format!(
            "{} {why}: each folder of the exchange path must be owned by root or this user and not writable by group or others (a sticky folder such as /tmp may be, when the folder in it is owned by root or this user)",
            at.display()
        )))
    }

    /// Check the trust rule on `dirs`, the pinned folders of `path` from
    /// `/` down, by their descriptors.
    pub fn check_trust(dirs: &[&OwnedFd], path: &Path) -> io::Result<()> {
        let mut names: Vec<&Path> = path.ancestors().collect();
        names.reverse();
        if names.len() != dirs.len() {
            return Err(io::Error::other(
                "the exchange path does not match its pinned folders",
            ));
        }
        let stats = dirs
            .iter()
            .map(|d| stat(d))
            .collect::<io::Result<Vec<_>>>()?;
        for (i, (st, at)) in stats.iter().zip(names).enumerate() {
            check_trusted(st, stats.get(i + 1).map(|s| s.st_uid), at)?;
        }
        Ok(())
    }

    /// Open each folder of `path` from `/` with no-follow opens, and with
    /// `create` make missing ones (0700) in the same walk. Any link in the
    /// walk is refused, so `path` comes from `resolve`. Each folder must
    /// meet the trust rule (taking the folder below as this user's; the
    /// caller checks the whole chain) before anything is made in it.
    pub fn pin(path: &Path, create: bool) -> io::Result<Vec<OwnedFd>> {
        let mut parts = path.components();
        if parts.next() != Some(Component::RootDir) {
            return Err(io::Error::other(
                "the exchange folder must be an absolute path",
            ));
        }
        let mut at = PathBuf::from("/");
        let root = open_at(libc::AT_FDCWD, c"/", DIR_FLAGS, 0)?;
        check_trusted(&stat(&root)?, Some(euid()), &at)?;
        let mut pins = vec![root];
        for part in parts {
            let Component::Normal(name) = part else {
                return Err(io::Error::other("the exchange path holds . or .."));
            };
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("the exchange path is not valid Unicode"))?;
            let parent = pins.last().expect("root").as_raw_fd();
            let c = name_c(name)?;
            if create {
                // SAFETY: `c` is NUL-terminated. mkdirat never follows a
                // symlink at the last component.
                if unsafe { libc::mkdirat(parent, c.as_ptr(), 0o700) } != 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        return Err(e);
                    }
                }
            }
            let fd = open_at(parent, &c, DIR_FLAGS, 0)?;
            at.push(name);
            check_trusted(&stat(&fd)?, Some(euid()), &at)?;
            pins.push(fd);
        }
        Ok(pins)
    }

    /// `path` with its longest existing prefix resolved by `realpath` and
    /// the missing rest appended unchanged. A symlink there (such as `/var`
    /// on macOS) is trusted as it stands now; `JobDir::check_path` catches
    /// a later change before each turn.
    pub fn resolve(path: &Path) -> io::Result<PathBuf> {
        let mut parts = path.components();
        if parts.next() != Some(Component::RootDir) {
            return Err(io::Error::other(
                "the exchange folder must be an absolute path",
            ));
        }
        if !parts.all(|p| matches!(p, Component::Normal(_))) {
            return Err(io::Error::other("the exchange path holds . or .."));
        }
        let mut base = path;
        let mut missing = Vec::new();
        loop {
            match std::fs::canonicalize(base) {
                Ok(mut real) => {
                    real.extend(missing.iter().rev());
                    return Ok(real);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let (Some(parent), Some(name)) = (base.parent(), base.file_name()) else {
                        return Err(e);
                    };
                    missing.push(name);
                    base = parent;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// True when `path`, looked up by name now, is the folder `fd`.
    pub fn same_folder(fd: &OwnedFd, path: &Path) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let by_path = std::fs::metadata(path)?;
        let pinned = File::from(fd.try_clone()?).metadata()?;
        Ok(by_path.dev() == pinned.dev() && by_path.ino() == pinned.ino())
    }

    pub fn open_dir(parent: &OwnedFd, name: &str) -> io::Result<Option<OwnedFd>> {
        match open_at(parent.as_raw_fd(), &name_c(name)?, DIR_FLAGS, 0) {
            Ok(fd) => Ok(Some(fd)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn descriptor(fd: &OwnedFd) -> io::Result<Descriptor> {
        let st = stat(fd)?;
        Ok(Descriptor {
            uid: st.st_uid,
            mode: mode_bits(&st),
        })
    }

    pub fn check_exchange(fd: &OwnedFd, _sid: Option<&str>) -> io::Result<()> {
        let d = descriptor(fd)?;
        let uid = euid();
        if d.uid != uid || d.mode & 0o077 != 0 {
            return Err(io::Error::other(format!(
                "the exchange folder must be owned by this user with mode 0700 (uid {}, mode {:o}); delete it and the watcher creates it again",
                d.uid, d.mode
            )));
        }
        Ok(())
    }

    pub fn valid_sid(_sid: &str) -> bool {
        true
    }

    pub fn create_exchange(parent: &OwnedFd, name: &str, sid: Option<&str>) -> io::Result<OwnedFd> {
        let c = name_c(name)?;
        // SAFETY: `c` is NUL-terminated.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = open_at(parent.as_raw_fd(), &c, DIR_FLAGS, 0)?;
        check_exchange(&fd, sid)?;
        Ok(fd)
    }

    pub fn create_job(exchange: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
        let c = name_c(name)?;
        // SAFETY: `c` is NUL-terminated.
        if unsafe { libc::mkdirat(exchange.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        open_at(exchange.as_raw_fd(), &c, DIR_FLAGS, 0)
    }

    pub fn write_new(dir: &OwnedFd, name: &str, bytes: &[u8]) -> io::Result<()> {
        let fd = open_at(
            dir.as_raw_fd(),
            &name_c(name)?,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let mut f = File::from(fd);
        f.write_all(bytes)?;
        f.sync_all()
    }

    pub fn read_capped(dir: &OwnedFd, name: &str, cap: usize) -> FileRead {
        let Ok(c) = name_c(name) else {
            return FileRead::Refused(format!("invalid name {name:?}"));
        };
        let fd = match open_at(dir.as_raw_fd(), &c, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return FileRead::Missing,
            Err(e) => return FileRead::Refused(format!("open {name}: {e}")),
        };
        match stat(&fd) {
            Ok(st) if st.st_mode & libc::S_IFMT == libc::S_IFREG && st.st_nlink == 1 => {}
            Ok(_) => {
                return FileRead::Refused(format!("{name} is not a regular file with one link"));
            }
            Err(e) => return FileRead::Refused(format!("read {name}: {e}")),
        }
        let mut bytes = Vec::new();
        if let Err(e) = File::from(fd).take(cap as u64 + 1).read_to_end(&mut bytes) {
            return FileRead::Refused(format!("read {name}: {e}"));
        }
        if bytes.len() > cap {
            return FileRead::TooBig;
        }
        FileRead::Bytes(bytes)
    }

    pub fn list(dir: &OwnedFd) -> io::Result<Vec<Entry>> {
        // fdopendir takes ownership, so give it a duplicate.
        // SAFETY: `dir` is open.
        let dup = unsafe { libc::dup(dir.as_raw_fd()) };
        if dup < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `dup` is a directory descriptor we own.
        let d = unsafe { libc::fdopendir(dup) };
        if d.is_null() {
            // SAFETY: fdopendir failed, so `dup` is still ours.
            unsafe { libc::close(dup) };
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `d` is open; rewind in case the duplicate shares a position.
        unsafe { libc::rewinddir(d) };
        let mut out = Vec::new();
        loop {
            // SAFETY: `d` is open.
            let e = unsafe { libc::readdir(d) };
            if e.is_null() {
                break;
            }
            // SAFETY: readdir returned a valid entry.
            let (name, kind) = unsafe { (CStr::from_ptr((*e).d_name.as_ptr()), (*e).d_type) };
            let name = name.to_string_lossy().into_owned();
            if name != "." && name != ".." {
                out.push(Entry {
                    name,
                    dir: kind == libc::DT_DIR,
                    link: kind == libc::DT_LNK,
                });
            }
        }
        // SAFETY: `d` is open and closed once.
        unsafe { libc::closedir(d) };
        Ok(out)
    }

    pub fn remove_dir(parent: &OwnedFd, name: &str, _dir: OwnedFd) -> io::Result<()> {
        let c = name_c(name)?;
        // SAFETY: `c` is NUL-terminated.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn remove_child(
        dir: &OwnedFd,
        name: &str,
        depth: usize,
        budget: &mut usize,
    ) -> io::Result<()> {
        if *budget == 0 {
            return Err(io::Error::other(
                "more than 1000 entries to remove; refused",
            ));
        }
        *budget -= 1;
        let c = name_c(name)?;
        match open_at(dir.as_raw_fd(), &c, DIR_FLAGS, 0) {
            Ok(sub) => {
                if depth + 1 >= CLEANUP_MAX_DEPTH {
                    return Err(io::Error::other("folders nested more than 8 deep; refused"));
                }
                remove_contents(&sub, depth + 1, budget)?;
                // SAFETY: `c` is NUL-terminated.
                if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Err(_) => {
                // A file or a link: unlinkat removes the link itself.
                // SAFETY: `c` is NUL-terminated.
                if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    pub fn remove_contents(dir: &OwnedFd, depth: usize, budget: &mut usize) -> io::Result<()> {
        for entry in list(dir)? {
            remove_child(dir, &entry.name, depth, budget)?;
        }
        Ok(())
    }
}

/// True when `sid` is accepted as `--agy-sandbox-sid` (Windows parses it).
pub fn valid_sid(sid: &str) -> bool {
    imp::valid_sid(sid)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use super::{Exchange, imp};

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_symlink_above_the_exchange_folder_is_resolved_once() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        // Whatever the umask, so the trust rule holds.
        chmod(&real, 0o700);
        symlink(&real, tmp.path().join("link")).unwrap();
        let alias = tmp.path().join("link").join("a").join("ex");
        // The walk itself follows no link.
        assert!(Exchange::open_or_create(&alias, None).is_err());
        assert!(!real.join("a").exists());
        // Missing folders below the link are made at its target, 0700.
        let path = Exchange::resolve(&alias).unwrap();
        let canonical_real = fs::canonicalize(&real).unwrap();
        assert_eq!(path, canonical_real.join("a").join("ex"));
        let ex = Exchange::open_or_create(&path, None).unwrap();
        assert_eq!(ex.path(), path.as_path());
        let mode = fs::metadata(real.join("a")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(real.join("a").join("ex").is_dir());
        ex.create_job("j1").unwrap();
        assert!(real.join("a").join("ex").join("j1").is_dir());
        drop(ex);
        // The exchange folder itself must not be a link.
        symlink(real.join("a").join("ex"), tmp.path().join("exlink")).unwrap();
        let exlink = fs::canonicalize(tmp.path()).unwrap().join("exlink");
        assert!(Exchange::open_or_create(&exlink, None).is_err());
    }

    #[test]
    fn a_job_folder_path_that_leads_elsewhere_is_caught() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let ex = Exchange::open_or_create(&base.join("a").join("ex"), None).unwrap();
        let job = ex.create_job("j1").unwrap();
        job.check_path().unwrap();
        // The pinned folder moves away and another takes its path.
        fs::rename(base.join("a"), base.join("b")).unwrap();
        fs::create_dir_all(base.join("a").join("ex").join("j1")).unwrap();
        assert!(job.check_path().is_err());
        // A link back to the pinned folder still leads to it.
        fs::remove_dir_all(base.join("a")).unwrap();
        symlink(base.join("b"), base.join("a")).unwrap();
        job.check_path().unwrap();
    }

    #[test]
    fn a_folder_above_the_exchange_that_others_can_write_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let shared = base.join("shared");
        fs::create_dir(&shared).unwrap();
        for mode in [0o770, 0o707, 0o777] {
            chmod(&shared, mode);
            let e = Exchange::open_or_create(&shared.join("a").join("ex"), None)
                .err()
                .expect("refused");
            assert!(e.to_string().contains("writable by group or others"), "{e}");
            // Refused before anything is made in it.
            assert!(!shared.join("a").exists());
        }
        // Not writable by others: accepted.
        chmod(&shared, 0o755);
        Exchange::open_or_create(&shared.join("a").join("ex"), None).unwrap();
    }

    #[test]
    fn a_sticky_shared_folder_with_a_folder_of_this_user_in_it_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let sticky = base.join("sticky");
        fs::create_dir(&sticky).unwrap();
        chmod(&sticky, 0o1777);
        let ex = Exchange::open_or_create(&sticky.join("a").join("ex"), None).unwrap();
        let job = ex.create_job("j1").unwrap();
        ex.check_trust(&job).unwrap();
        // Sticky, but the folder in it is another user's: refused. (Only
        // root can make such a folder, so the stat is made up.)
        // SAFETY: no arguments.
        let me = unsafe { libc::geteuid() };
        let other = if me == 4242 { 4243 } else { 4242 };
        // SAFETY: an all-zero stat is a valid value.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        st.st_uid = me;
        st.st_mode = libc::S_IFDIR | 0o1777;
        imp::check_trusted(&st, Some(me), &sticky).unwrap();
        imp::check_trusted(&st, Some(0), &sticky).unwrap();
        let e = imp::check_trusted(&st, Some(other), &sticky).unwrap_err();
        assert!(e.to_string().contains("sticky"), "{e}");
        // The last folder of the path may not be shared, sticky or not.
        assert!(imp::check_trusted(&st, None, &sticky).is_err());
        // A folder owned by another user is refused whatever its mode.
        st.st_uid = other;
        st.st_mode = libc::S_IFDIR | 0o755;
        assert!(imp::check_trusted(&st, Some(me), &sticky).is_err());
    }

    /// Holding descriptors does not stop renames on Unix. A folder above
    /// the job folder that others can write would let them swap the path
    /// after the identity check and before agy opens the request; the
    /// check before each turn refuses that setup.
    #[test]
    fn an_ancestor_opened_to_others_after_start_fails_the_turn_check() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let ex = Exchange::open_or_create(&base.join("a").join("ex"), None).unwrap();
        let job = ex.create_job("j1").unwrap();
        ex.check_trust(&job).unwrap();
        for mode in [0o770, 0o777] {
            chmod(&base.join("a"), mode);
            let e = ex.check_trust(&job).unwrap_err();
            assert!(e.to_string().contains("writable by group or others"), "{e}");
        }
        // Sticky with this user's folder in it: no one else can rename it.
        chmod(&base.join("a"), 0o1777);
        ex.check_trust(&job).unwrap();
        // The check reads the pinned folders, not the names: a folder that
        // others could write and that moved away is still caught.
        chmod(&base.join("a"), 0o777);
        fs::rename(base.join("a"), base.join("b")).unwrap();
        fs::create_dir_all(base.join("a").join("ex").join("j1")).unwrap();
        assert!(ex.check_trust(&job).is_err());
        // The job folder itself is checked too.
        chmod(&base.join("b"), 0o700);
        ex.check_trust(&job).unwrap();
        chmod(&base.join("b").join("ex").join("j1"), 0o770);
        assert!(ex.check_trust(&job).is_err());
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::fs;
    use std::os::windows::io::OwnedHandle;
    use std::path::Path;
    use std::process::Command;

    use super::imp::{
        self, Ace, Descriptor, INHERIT_FLAGS, INHERITED_FLAG, MODIFY, TRAVERSE_GRANT, allow,
        low_label,
    };
    use super::{Exchange, FileRead};
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    const SID: &str = "S-1-15-3-1024-1-2-3-4-5-6-7-8";

    fn user() -> String {
        imp::current_user().unwrap()
    }

    fn descriptor_of(path: &Path) -> Descriptor {
        let f = fs::File::open(path).unwrap();
        imp::descriptor(&OwnedHandle::from(f)).unwrap()
    }

    #[test]
    fn session_trust_refuses_every_sandbox_principal_and_no_other() {
        let with = |ace: Ace| Descriptor {
            owner: user(),
            protected: false,
            dacl: Some(vec![
                allow(FILE_ALL_ACCESS, INHERIT_FLAGS, imp::SYSTEM_SID),
                ace,
            ]),
            label: vec![],
        };
        let listed = ["S-1-5-21-1-2-3-1001".to_string()];
        // An inherit-only grant on the root reaches every folder below it.
        let inherit_only = INHERIT_FLAGS | 0x8;
        for sid in [
            SID,
            "S-1-15-3-1024-9-9-9",
            "S-1-15-2-1",
            "S-1-15-2-2",
            imp::LOW_LABEL_SID,
            "S-1-5-12",
            "S-1-5-33",
            listed[0].as_str(),
        ] {
            for flags in [0, INHERIT_FLAGS, inherit_only] {
                let why = imp::untrusted(&with(allow(MODIFY, flags, sid)), &listed);
                assert!(why.is_some_and(|w| w.contains(sid)), "{sid} {flags}");
            }
        }
        // Normal-user trust: the owner, users, administrators and SYSTEM.
        for sid in [
            user().as_str(),
            "S-1-5-11",
            "S-1-5-32-545",
            "S-1-5-32-544",
            imp::SYSTEM_SID,
        ] {
            let d = with(allow(MODIFY, INHERIT_FLAGS, sid));
            assert_eq!(imp::untrusted(&d, &listed), None, "{sid}");
        }
        // Read access for a sandbox SID is fine.
        let read = with(allow(0x1200a9, INHERIT_FLAGS, "S-1-15-2-1"));
        assert_eq!(imp::untrusted(&read, &listed), None);
    }

    fn junction(link: &Path, target: &Path) {
        let ok = Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J failed");
    }

    /// A folder at `parent/name` with `dacl` (protected) and, if `label`,
    /// the Low label.
    fn folder_with(parent: &Path, name: &str, dacl: &[Ace], protected: bool, label: bool) {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        };
        let open = |access| {
            let f = fs::OpenOptions::new()
                .access_mode(access)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(parent.join(name))
                .unwrap();
            OwnedHandle::from(f)
        };
        fs::create_dir(parent.join(name)).unwrap();
        // The owner has WRITE_DAC; WRITE_OWNER (for the label) comes from
        // the new DACL's Full control grant.
        imp::set_dacl(&open(READ_CONTROL | WRITE_DAC), dacl, protected).unwrap();
        if label {
            imp::set_low_label(&open(READ_CONTROL | WRITE_OWNER)).unwrap();
        }
    }

    #[test]
    fn a_new_exchange_folder_has_the_contract_and_children_inherit_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ex");
        let ex = Exchange::open_or_create(&path, Some(SID)).unwrap();
        let want = Descriptor::contract(&user(), SID);
        let have = imp::descriptor(&ex.dir).unwrap();
        assert_eq!(have.mismatch(&want), None, "{have:?}");

        let job = ex.create_job("j1").unwrap();
        let d = job.descriptor().unwrap();
        assert!(d.is_low(), "{d:?}");
        assert!(!d.protected);
        // The INHERITED_ACE bit is set only when the parent DACL is marked
        // auto-inherited, so it is not compared.
        let strip = |v: Vec<Ace>| {
            let mut v: Vec<Ace> = v
                .into_iter()
                .map(|mut a| {
                    a.flags &= !INHERITED_FLAG;
                    a
                })
                .collect();
            v.sort();
            v
        };
        let flags = INHERIT_FLAGS;
        let got = strip(d.dacl.clone().unwrap());
        let mut exp = vec![
            allow(FILE_ALL_ACCESS, flags, &user()),
            allow(FILE_ALL_ACCESS, flags, imp::SYSTEM_SID),
            allow(MODIFY, flags, SID),
        ];
        exp.sort();
        assert_eq!(got, exp);
        assert_eq!(strip(d.label.clone()), vec![low_label(flags)]);

        job.write_new("request.json", b"{}").unwrap();
        let f = descriptor_of(&job.path().join("request.json"));
        assert_eq!(strip(f.label.clone()), vec![low_label(0)], "{f:?}");
        assert_eq!(f.dacl.unwrap().len(), 3);
        assert!(
            job.write_new("request.json", b"{}").is_err(),
            "create must fail if it exists"
        );

        // A file renamed in keeps its own (Medium) label.
        let outside = tmp.path().join("outside.json");
        fs::write(&outside, b"{}").unwrap();
        fs::rename(&outside, job.path().join("moved.json")).unwrap();
        assert!(!descriptor_of(&job.path().join("moved.json")).is_low());

        let names: Vec<_> = ex
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.dir))
            .collect();
        assert_eq!(names, vec![("j1".to_owned(), true)]);
        ex.remove_job(job).unwrap();
        assert!(!path.join("j1").exists());
        assert!(ex.entries().unwrap().is_empty());
    }

    #[test]
    fn an_existing_folder_is_compared_as_a_set() {
        let tmp = tempfile::tempdir().unwrap();
        let u = user();
        let good = [
            allow(MODIFY, INHERIT_FLAGS, SID),
            allow(FILE_ALL_ACCESS, INHERIT_FLAGS, imp::SYSTEM_SID),
            allow(FILE_ALL_ACCESS, INHERIT_FLAGS, &u),
        ];
        // fs::create_dir gives the token's default owner: the user, or
        // BUILTIN\Administrators when the tests run elevated.
        folder_with(tmp.path(), "reordered", &good, true, true);
        let ex = Exchange::open_or_create(&tmp.path().join("reordered"), Some(SID)).unwrap();
        let owner = imp::descriptor(&ex.dir).unwrap().owner;
        assert_eq!(owner, imp::token_owner().unwrap());

        let mut missing_flag = good.clone();
        missing_flag[0].flags = OBJECT_INHERIT;
        folder_with(tmp.path(), "flag", &missing_flag, true, true);
        let mut extra = good.to_vec();
        extra.push(allow(FILE_ALL_ACCESS, INHERIT_FLAGS, "S-1-1-0"));
        folder_with(tmp.path(), "extra", &extra, true, true);
        folder_with(tmp.path(), "unprotected", &good, false, true);
        folder_with(tmp.path(), "nolabel", &good, true, false);

        // The entry agy's sandbox adds when its shell starts is accepted;
        // anything wider, inheritable or for another SID is not.
        let with = |name: &str, ace: Ace| {
            let mut dacl = good.to_vec();
            dacl.push(ace);
            folder_with(tmp.path(), name, &dacl, true, true);
        };
        with("traverse", allow(TRAVERSE_GRANT, 0, SID));
        Exchange::open_or_create(&tmp.path().join("traverse"), Some(SID)).unwrap();
        with("traverse-other", allow(TRAVERSE_GRANT, 0, "S-1-1-0"));
        with(
            "traverse-inherit",
            allow(TRAVERSE_GRANT, INHERIT_FLAGS, SID),
        );
        with("traverse-read", allow(TRAVERSE_GRANT | 1, 0, SID));
        for name in [
            "flag",
            "extra",
            "unprotected",
            "nolabel",
            "traverse-other",
            "traverse-inherit",
            "traverse-read",
        ] {
            let e = Exchange::open_or_create(&tmp.path().join(name), Some(SID))
                .err()
                .unwrap_or_else(|| panic!("{name} accepted"));
            assert!(e.to_string().contains("expected access"), "{name}: {e}");
        }
        // The SID must be given and valid.
        assert!(Exchange::open_or_create(&tmp.path().join("new"), None).is_err());
        assert!(Exchange::open_or_create(&tmp.path().join("new"), Some("not-a-sid")).is_err());
        assert!(!tmp.path().join("new").exists());
    }

    const OBJECT_INHERIT: u8 = 1;

    #[test]
    fn descriptor_mismatch_ignores_order_only() {
        let want = Descriptor::contract("S-1-5-21-1-2-3-1001", SID);
        let mut d = want.clone();
        d.dacl.as_mut().unwrap().reverse();
        assert_eq!(d.mismatch(&want), None);
        let mut wider = want.clone();
        wider.dacl.as_mut().unwrap()[2].mask = FILE_ALL_ACCESS;
        assert!(wider.mismatch(&want).is_some());
        let mut owner = want.clone();
        owner.owner = "S-1-5-32-544".into();
        assert!(owner.mismatch(&want).unwrap().contains("owner"));
        let mut null = want.clone();
        null.dacl = None;
        assert!(null.mismatch(&want).is_some());
        let mut two = want.clone();
        two.label.push(low_label(INHERIT_FLAGS));
        assert!(two.mismatch(&want).is_some());
    }

    #[test]
    fn resolve_gives_the_real_long_name_and_makes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("Long Folder Name");
        fs::create_dir(&real).unwrap();
        let want = Exchange::resolve(&real).unwrap();
        assert!(want.ends_with("Long Folder Name"), "{}", want.display());
        assert!(!want.to_string_lossy().starts_with(r"\\?\"));
        let other_case = tmp.path().join("LONG FOLDER NAME").join("a").join("ex");
        assert_eq!(
            Exchange::resolve(&other_case).unwrap(),
            want.join("a").join("ex")
        );
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        // A junction is refused, not resolved.
        junction(&tmp.path().join("jn"), &real);
        assert!(Exchange::resolve(&tmp.path().join("jn").join("ex")).is_err());
    }

    #[test]
    fn junctions_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        // At an ancestor of the exchange folder.
        junction(&tmp.path().join("jn"), &real);
        let e = Exchange::open_or_create(&tmp.path().join("jn").join("ex"), Some(SID))
            .err()
            .expect("an ancestor junction must be refused");
        assert!(e.to_string().contains("reparse point"), "{e}");
        assert!(!real.join("ex").exists());
        // With missing folders below it: none is made at the target.
        let deep = tmp.path().join("jn").join("a").join("b").join("ex");
        let e = Exchange::open_or_create(&deep, Some(SID))
            .err()
            .expect("an ancestor junction must be refused");
        assert!(e.to_string().contains("reparse point"), "{e}");
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);

        let ex = Exchange::open_or_create(&tmp.path().join("ex"), Some(SID)).unwrap();
        // At a job folder.
        junction(&ex.path().join("j2"), &real);
        assert!(ex.open_job("j2").is_err());
        // At reply.json.
        let job = ex.create_job("j1").unwrap();
        junction(&job.path().join("reply.json"), &real);
        assert!(matches!(
            job.read_capped("reply.json", 100),
            FileRead::Refused(_)
        ));
        // A junction inside a job folder is deleted, not followed.
        fs::write(real.join("keep.txt"), b"x").unwrap();
        junction(&job.path().join("sub"), &real);
        ex.remove_job(job).unwrap();
        assert!(real.join("keep.txt").exists());
        assert!(!ex.path().join("j1").exists());
        // A link in the exchange folder itself is removed as a link.
        ex.remove_entry("j2").unwrap();
        assert!(real.join("keep.txt").exists());
    }

    #[test]
    fn pinned_folders_cannot_be_renamed() {
        let tmp = tempfile::tempdir().unwrap();
        let mid = tmp.path().join("mid");
        let ex = Exchange::open_or_create(&mid.join("ex"), Some(SID)).unwrap();
        let job = ex.create_job("j1").unwrap();
        assert!(fs::rename(job.path(), ex.path().join("j1x")).is_err());
        assert!(fs::rename(ex.path(), mid.join("ex2")).is_err());
        assert!(fs::rename(&mid, tmp.path().join("mid2")).is_err());
        drop(job);
        drop(ex);
        fs::rename(&mid, tmp.path().join("mid2")).unwrap();
    }

    /// agy's sandboxed shell opens each folder of its `--add-dir` path
    /// without delete sharing and adds an entry to its DACL. The held
    /// handles must not make that fail, and the exchange folder must still
    /// be accepted afterwards.
    #[test]
    fn the_agy_sandbox_can_add_its_entry_to_pinned_folders() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_READ,
            FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC,
        };
        let tmp = tempfile::tempdir().unwrap();
        let mid = tmp.path().join("mid");
        let ex = Exchange::open_or_create(&mid.join("ex"), Some(SID)).unwrap();
        let job = ex.create_job("j1").unwrap();
        let open = |path: &Path| {
            fs::OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY | FILE_ADD_FILE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .map(OwnedHandle::from)
        };
        for path in [job.path(), ex.path(), &mid] {
            open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        let mut dacl = Descriptor::contract(&user(), SID).dacl.unwrap();
        dacl.push(allow(TRAVERSE_GRANT, 0, SID));
        imp::set_dacl(&open(ex.path()).unwrap(), &dacl, true).unwrap();
        imp::set_dacl(&open(job.path()).unwrap(), &dacl, false).unwrap();
        // Still pinned: no folder of the path can be renamed.
        assert!(fs::rename(job.path(), ex.path().join("j1x")).is_err());
        assert!(fs::rename(ex.path(), mid.join("ex2")).is_err());
        ex.remove_job(job).unwrap();
        assert!(!ex.path().join("j1").exists());
        drop(ex);
        Exchange::open_or_create(&mid.join("ex"), Some(SID)).unwrap();
    }

    /// The same with the real `agy`: its shell starts in a pinned job
    /// folder. A live test, run by hand: set `CQ_LIVE_AGY_BASE` to an
    /// empty scratch folder and `CQ_LIVE_AGY_SID` to the sandbox SID, then
    /// `cargo test --lib live_agy -- --ignored --nocapture`. Each run is
    /// one agy print turn.
    #[test]
    #[ignore = "runs the real agy"]
    fn live_agy_shell_starts_in_a_pinned_job_folder() {
        let (Ok(base), Ok(sid)) = (
            std::env::var("CQ_LIVE_AGY_BASE"),
            std::env::var("CQ_LIVE_AGY_SID"),
        ) else {
            eprintln!("CQ_LIVE_AGY_BASE or CQ_LIVE_AGY_SID not set; skipped");
            return;
        };
        let path = Exchange::resolve(&Path::new(&base).join("ex")).unwrap();
        let ex = Exchange::open_or_create(&path, Some(&sid)).unwrap();
        let job = ex.create_job("j1").unwrap();
        job.write_new("request.json", b"{}").unwrap();
        let dir = job.path().display().to_string();
        let out = Command::new("agy")
            .current_dir(job.path())
            .args(["--add-dir", &dir, "-p"])
            .arg(format!(
                "Use only your shell tool, not a file tool: in {dir} run a command that creates shell.txt containing hi. If the shell tool fails, quote its error exactly and create nothing."
            ))
            .args(["--output-format", "json"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        eprintln!("{text}");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !text.contains("exebox"),
            "the sandboxed shell did not start"
        );
        assert!(job.path().join("shell.txt").exists());
        eprintln!(
            "exchange folder after the turn: {:?}",
            imp::descriptor(&ex.dir)
        );
        eprintln!("job folder after the turn: {:?}", job.descriptor());
        ex.remove_job(job).unwrap();
        drop(ex);
        Exchange::open_or_create(&path, Some(&sid)).unwrap();
    }

    #[test]
    fn reads_are_capped_and_need_a_closed_single_link_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ex = Exchange::open_or_create(&tmp.path().join("ex"), Some(SID)).unwrap();
        let job = ex.create_job("j1").unwrap();
        let reply = job.path().join("reply.json");
        assert_eq!(job.read_capped("reply.json", 10), FileRead::Missing);
        fs::write(&reply, b"0123456789").unwrap();
        assert_eq!(
            job.read_capped("reply.json", 10),
            FileRead::Bytes(b"0123456789".to_vec())
        );
        fs::write(&reply, vec![b'x'; 65 * 1024]).unwrap();
        assert_eq!(job.read_capped("reply.json", 64 * 1024), FileRead::TooBig);
        // A writer that still holds the file open: refused, not read.
        let writer = fs::OpenOptions::new().append(true).open(&reply).unwrap();
        assert!(matches!(
            job.read_capped("reply.json", 1 << 20),
            FileRead::Refused(_)
        ));
        drop(writer);
        // A second hard link.
        fs::remove_file(&reply).unwrap();
        fs::write(tmp.path().join("other"), b"{}").unwrap();
        fs::hard_link(tmp.path().join("other"), &reply).unwrap();
        assert!(matches!(
            job.read_capped("reply.json", 100),
            FileRead::Refused(_)
        ));
        // A folder named reply.json.
        fs::remove_file(&reply).unwrap();
        fs::create_dir(&reply).unwrap();
        assert!(matches!(
            job.read_capped("reply.json", 100),
            FileRead::Refused(_)
        ));
    }

    #[test]
    fn the_label_needs_write_owner_and_a_job_folder_needs_a_low_parent() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("plain")).unwrap();
        let pins = imp::pin(tmp.path(), false).unwrap();
        let h = imp::open_dir(pins.last().unwrap(), "plain")
            .unwrap()
            .unwrap();
        assert!(imp::set_low_label(&h).is_err());
        let e = imp::create_job(&h, "j1").expect_err("a Medium parent must be refused");
        assert!(e.to_string().contains("Low label"), "{e}");
        drop(h);
        assert!(!tmp.path().join("plain").join("j1").exists());
    }
}
