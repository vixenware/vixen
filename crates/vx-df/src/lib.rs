//! Parser for `df` command output on macOS/Linux.

/// A single entry from `df` output.
#[derive(Debug, Clone)]
pub struct DfEntry {
    /// The filesystem field (e.g. "/dev/disk3s1s1", "fskitty://127.0.0.1:53953")
    pub filesystem: String,

    /// The mount point (last column)
    pub mount_point: String,
}

/// A parsed fskitty URL from a df filesystem field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsKittyUrl {
    pub host: String,
    pub port: u16,
}

impl DfEntry {
    /// If this entry's filesystem is a `fskitty://` URL, parse it.
    pub fn as_fskitty_url(&self) -> Option<FsKittyUrl> {
        let rest = self.filesystem.strip_prefix("fskitty://")?;
        let (host, port_str) = rest.rsplit_once(':')?;
        let port = port_str.parse::<u16>().ok()?;
        Some(FsKittyUrl {
            host: host.to_string(),
            port,
        })
    }
}

/// Parse the output of `df` into structured entries.
///
/// Expects the standard format with a header line followed by data lines.
/// The filesystem is the first whitespace-delimited field, the mount point
/// is everything after the last numeric/percentage column.
pub fn parse_df(output: &str) -> Vec<DfEntry> {
    let mut lines = output.lines();

    // Skip header line
    let Some(header) = lines.next() else {
        return vec![];
    };

    // Find where "Mounted on" starts in the header — mount points with spaces
    // are only reliably parsed by using the header's column position.
    let mount_col = header.find("Mounted on").unwrap_or(0);

    lines
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }

            let filesystem = line.split_whitespace().next()?;

            // Mount point: use the header column offset if the line is long enough,
            // otherwise fall back to last whitespace-delimited token.
            let mount_point = if line.len() > mount_col && mount_col > 0 {
                line[mount_col..].trim()
            } else {
                line.split_whitespace().last()?
            };

            Some(DfEntry {
                filesystem: filesystem.to_string(),
                mount_point: mount_point.to_string(),
            })
        })
        .collect()
}

/// Find a fskitty mount at the given mount point with the expected host and port.
pub fn find_fskitty_mount(
    df_output: &str,
    expected_mount_point: &str,
    expected_host: &str,
    expected_port: u16,
) -> bool {
    for entry in parse_df(df_output) {
        // Mount points may be canonicalized (e.g. /tmp -> /private/tmp on macOS)
        if !entry.mount_point.ends_with(expected_mount_point)
            && expected_mount_point != entry.mount_point
        {
            continue;
        }
        if let Some(url) = entry.as_fskitty_url() {
            if url.host == expected_host && url.port == expected_port {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DF: &str = "\
Filesystem                512-blocks       Used Available Capacity iused      ifree %iused  Mounted on
/dev/disk3s1s1            1942700360   23981152 897136872     3%  453019 4293306490    0%   /
devfs                            420        420         0   100%     727          0  100%   /dev
/dev/disk3s6              1942700360    2097200 897136872     1%       1 4485684360    0%   /System/Volumes/VM
/dev/disk3s2              1942700360   16001552 897136872     2%    1348 4485684360    0%   /System/Volumes/Preboot
/dev/disk3s4              1942700360      17504 897136872     1%     108 4485684360    0%   /System/Volumes/Update
/dev/disk1s2                 1024000      12328    985344     2%       1    4926720    0%   /System/Volumes/xarts
/dev/disk1s1                 1024000      11544    985344     2%      31    4926720    0%   /System/Volumes/iSCPreboot
/dev/disk1s3                 1024000       5064    985344     1%      91    4926720    0%   /System/Volumes/Hardware
/dev/disk3s5              1942700360 1000636912 897136872    53% 5788945 4485684360    0%   /System/Volumes/Data
map auto_home                      0          0         0   100%       0          0     -   /System/Volumes/Data/home
fskitty://127.0.0.1:53953    8000000          0   4000000     0%   50000      50000   50%   /private/tmp/vx-debug/fskitty/rhea";

    #[test]
    fn parse_all_entries() {
        let entries = parse_df(SAMPLE_DF);
        assert_eq!(entries.len(), 11);
        assert_eq!(entries[0].filesystem, "/dev/disk3s1s1");
        assert_eq!(entries[0].mount_point, "/");
        assert_eq!(entries[10].filesystem, "fskitty://127.0.0.1:53953");
        assert_eq!(
            entries[10].mount_point,
            "/private/tmp/vx-debug/fskitty/rhea"
        );
    }

    #[test]
    fn parse_fskitty_url() {
        let entries = parse_df(SAMPLE_DF);
        let fskitty = entries[10].as_fskitty_url().unwrap();
        assert_eq!(fskitty.host, "127.0.0.1");
        assert_eq!(fskitty.port, 53953);
    }

    #[test]
    fn non_fskitty_has_no_url() {
        let entries = parse_df(SAMPLE_DF);
        assert!(entries[0].as_fskitty_url().is_none());
    }

    #[test]
    fn find_matching_mount() {
        assert!(find_fskitty_mount(
            SAMPLE_DF,
            "/tmp/vx-debug/fskitty/rhea",
            "127.0.0.1",
            53953,
        ));
    }

    #[test]
    fn wrong_port_no_match() {
        assert!(!find_fskitty_mount(
            SAMPLE_DF,
            "/tmp/vx-debug/fskitty/rhea",
            "127.0.0.1",
            9999,
        ));
    }

    #[test]
    fn wrong_mount_point_no_match() {
        assert!(!find_fskitty_mount(
            SAMPLE_DF,
            "/some/other/path",
            "127.0.0.1",
            53953,
        ));
    }
}
