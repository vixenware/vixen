use std::ffi::OsString;
use std::os::unix::io::RawFd;

use nix::unistd;

/// mount options.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct MountOptions {
    pub(crate) nonempty: bool,
    pub(crate) allow_other: bool,
    pub(crate) allow_root: bool,
    pub(crate) custom_options: Option<OsString>,
    pub(crate) dirsync: bool,
    pub(crate) default_permissions: bool,
    pub(crate) fs_name: Option<String>,
    pub(crate) gid: Option<u32>,
    pub(crate) nodiratime: bool,
    pub(crate) noatime: bool,
    pub(crate) nodev: bool,
    pub(crate) noexec: bool,
    pub(crate) nosuid: bool,
    pub(crate) read_only: bool,
    pub(crate) sync: bool,
    pub(crate) uid: Option<u32>,
    pub(crate) dont_mask: bool,
    pub(crate) no_open_support: bool,
    pub(crate) no_open_dir_support: bool,
    pub(crate) handle_killpriv: bool,
    pub(crate) write_back: bool,
    pub(crate) force_readdir_plus: bool,
    pub(crate) rootmode: Option<u32>,
}

impl MountOptions {
    pub fn uid(&mut self, uid: u32) -> &mut Self {
        self.uid.replace(uid);
        self
    }

    pub fn gid(&mut self, gid: u32) -> &mut Self {
        self.gid.replace(gid);
        self
    }

    pub fn fs_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.fs_name.replace(name.into());
        self
    }

    pub fn rootmode(&mut self, rootmode: u32) -> &mut Self {
        self.rootmode.replace(rootmode);
        self
    }

    pub fn allow_root(&mut self, allow_root: bool) -> &mut Self {
        self.allow_root = allow_root;
        self
    }

    pub fn allow_other(&mut self, allow_other: bool) -> &mut Self {
        self.allow_other = allow_other;
        self
    }

    pub fn read_only(&mut self, read_only: bool) -> &mut Self {
        self.read_only = read_only;
        self
    }

    pub fn nonempty(&mut self, nonempty: bool) -> &mut Self {
        self.nonempty = nonempty;
        self
    }

    pub fn default_permissions(&mut self, default_permissions: bool) -> &mut Self {
        self.default_permissions = default_permissions;
        self
    }

    pub fn dont_mask(&mut self, dont_mask: bool) -> &mut Self {
        self.dont_mask = dont_mask;
        self
    }

    pub fn no_open_support(&mut self, no_open_support: bool) -> &mut Self {
        self.no_open_support = no_open_support;
        self
    }

    pub fn no_open_dir_support(&mut self, no_open_dir_support: bool) -> &mut Self {
        self.no_open_dir_support = no_open_dir_support;
        self
    }

    pub fn handle_killpriv(&mut self, handle_killpriv: bool) -> &mut Self {
        self.handle_killpriv = handle_killpriv;
        self
    }

    pub fn write_back(&mut self, write_back: bool) -> &mut Self {
        self.write_back = write_back;
        self
    }

    pub fn force_readdir_plus(&mut self, force_readdir_plus: bool) -> &mut Self {
        self.force_readdir_plus = force_readdir_plus;
        self
    }

    pub fn custom_options(&mut self, custom_options: impl Into<OsString>) -> &mut Self {
        self.custom_options = Some(custom_options.into());
        self
    }

    pub(crate) fn build(&self, fd: RawFd) -> OsString {
        let mut opts = vec![
            format!("fd={fd}"),
            format!(
                "user_id={}",
                self.uid.unwrap_or_else(|| unistd::getuid().as_raw())
            ),
            format!(
                "group_id={}",
                self.gid.unwrap_or_else(|| unistd::getgid().as_raw())
            ),
            format!("rootmode={}", self.rootmode.unwrap_or(40000)),
        ];

        if self.allow_root {
            opts.push("allow_root".to_string());
        }
        if self.allow_other {
            opts.push("allow_other".to_string());
        }
        if self.default_permissions {
            opts.push("default_permissions".to_string());
        }

        let mut options = OsString::from(opts.join(","));
        if let Some(custom_options) = &self.custom_options {
            options.push(",");
            options.push(custom_options);
        }
        options
    }

    pub(crate) fn build_with_unprivileged(&self) -> OsString {
        let mut opts = vec![
            format!(
                "user_id={}",
                self.uid.unwrap_or_else(|| unistd::getuid().as_raw())
            ),
            format!(
                "group_id={}",
                self.gid.unwrap_or_else(|| unistd::getgid().as_raw())
            ),
            format!("rootmode={}", self.rootmode.unwrap_or(40000)),
            format!(
                "fsname={}",
                self.fs_name.as_ref().unwrap_or(&"fuse".to_string())
            ),
        ];

        if self.allow_root {
            opts.push("allow_root".to_string());
        }
        if self.allow_other {
            opts.push("allow_other".to_string());
        }
        if self.read_only {
            opts.push("ro".to_string());
        }
        if self.default_permissions {
            opts.push("default_permissions".to_string());
        }

        let mut options = OsString::from(opts.join(","));
        if let Some(custom_options) = &self.custom_options {
            options.push(",");
            options.push(custom_options);
        }
        options
    }

    pub(crate) fn flags(&self) -> nix::mount::MsFlags {
        use nix::mount::MsFlags;
        let mut flags = MsFlags::empty();
        if self.dirsync {
            flags.insert(MsFlags::MS_DIRSYNC);
        }
        if self.noatime {
            flags.insert(MsFlags::MS_NOATIME);
        }
        if self.nodev {
            flags.insert(MsFlags::MS_NODEV);
        }
        if self.nodiratime {
            flags.insert(MsFlags::MS_NODIRATIME);
        }
        if self.noexec {
            flags.insert(MsFlags::MS_NOEXEC);
        }
        if self.nosuid {
            flags.insert(MsFlags::MS_NOSUID);
        }
        if self.read_only {
            flags.insert(MsFlags::MS_RDONLY);
        }
        if self.sync {
            flags.insert(MsFlags::MS_SYNCHRONOUS);
        }
        flags
    }
}
