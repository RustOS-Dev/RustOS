//! Checks against real images made by e2fsprogs (skipped when mkfs.ext4 or
//! debugfs are not installed).

use ext4_core::{csum, jbd2};
use std::process::Command;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("-V").output().is_ok()
}

struct Image {
    data: Vec<u8>,
    bs: usize,
}

impl Image {
    fn make(name: &str, mkfs_args: &[&str], debugfs_cmds: Option<&str>) -> Option<Image> {
        if !tool("mkfs.ext4") || !tool("debugfs") {
            eprintln!("e2fsprogs missing: skipped");
            return None;
        }
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ext4core-{}-{}.img", name, std::process::id()));
        std::fs::write(&path, vec![0u8; 32 << 20]).unwrap();
        let st = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .args(mkfs_args)
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success());
        if let Some(cmds) = debugfs_cmds {
            let f = dir.join(format!("ext4core-{}-{}.cmds", name, std::process::id()));
            std::fs::write(&f, cmds).unwrap();
            let st = Command::new("debugfs")
                .arg("-w")
                .arg("-f")
                .arg(&f)
                .arg(&path)
                .output()
                .unwrap();
            assert!(st.status.success());
            let _ = std::fs::remove_file(&f);
        }
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let bs = 1024usize << le32(&data[1024..], 24);
        Some(Image { data, bs })
    }
    fn sb(&self) -> &[u8] {
        &self.data[1024..2048]
    }
    fn block(&self, b: u64) -> &[u8] {
        &self.data[b as usize * self.bs..(b as usize + 1) * self.bs]
    }
    fn desc_size(&self) -> usize {
        if le32(self.sb(), 0x60) & 0x80 != 0 {
            le16(self.sb(), 0xFE) as usize
        } else {
            32
        }
    }
    fn desc(&self, g: usize) -> &[u8] {
        let first = le32(self.sb(), 20) as usize;
        let off = (first + 1) * self.bs + g * self.desc_size();
        &self.data[off..off + self.desc_size()]
    }
    fn inode(&self, ino: u32) -> &[u8] {
        let ipg = le32(self.sb(), 40);
        let isz = le16(self.sb(), 88) as usize;
        let g = ((ino - 1) / ipg) as usize;
        let table = le32(self.desc(g), 8) as usize;
        let off = table * self.bs + ((ino - 1) % ipg) as usize * isz;
        &self.data[off..off + isz]
    }
    /// Physical block of logical block `l` of an extent-mapped inode
    /// (depth-0 trees only, enough for fresh images).
    fn bmap(&self, raw: &[u8], l: u32) -> u64 {
        let node = &raw[40..100];
        assert_eq!(le16(node, 0), 0xF30A);
        assert_eq!(
            le16(node, 6),
            0,
            "deeper extent trees not handled in the test"
        );
        for e in 0..le16(node, 2) as usize {
            let o = 12 + e * 12;
            let first = le32(node, o);
            let len = le16(node, o + 4) as u32;
            let start = (le16(node, o + 6) as u64) << 32 | le32(node, o + 8) as u64;
            if l >= first && l < first + len {
                return start + (l - first) as u64;
            }
        }
        panic!("unmapped block {}", l);
    }
}

#[test]
fn metadata_checksums_match_mkfs() {
    let Some(img) = Image::make(
        "csum",
        &["-O", "metadata_csum,64bit"],
        Some("mkdir d1\nmkdir d1/d2\n"),
    ) else {
        return;
    };
    let sb = img.sb();
    assert_eq!(csum::superblock(sb), le32(sb, 0x3FC), "superblock");
    let incompat = le32(sb, 0x60);
    let seed = csum::fs_seed(sb, incompat & 0x2000 != 0);
    let blocks = le32(sb, 4) as u64 | (le32(sb, 0x150) as u64) << 32;
    let bpg = le32(sb, 32) as u64;
    let ngroups = blocks.div_ceil(bpg) as usize;
    let ipg = le32(sb, 40);
    for g in 0..ngroups {
        let d = img.desc(g);
        assert_eq!(
            csum::group_desc(seed, g as u32, d),
            le16(d, 0x1E),
            "group {} descriptor",
            g
        );
        let flags = le16(d, 0x12);
        if flags & 0x2 == 0 {
            // BLOCK_UNINIT clear: the bitmap and its checksum are real.
            let bb = le32(d, 0) as u64 | (le32(d, 0x20) as u64) << 32;
            let c = csum::bitmap(seed, &img.block(bb)[..bpg as usize / 8]);
            let stored = le16(d, 0x18) as u32 | (le16(d, 0x38) as u32) << 16;
            assert_eq!(c, stored, "group {} block bitmap", g);
        }
        if flags & 0x1 == 0 {
            let ib = le32(d, 4) as u64 | (le32(d, 0x24) as u64) << 32;
            let c = csum::bitmap(seed, &img.block(ib)[..ipg as usize / 8]);
            let stored = le16(d, 0x1A) as u32 | (le16(d, 0x3A) as u32) << 16;
            assert_eq!(c, stored, "group {} inode bitmap", g);
        }
    }
    for ino in [2u32, 11, 12] {
        let raw = img.inode(ino);
        let generation = le32(raw, csum::INODE_GENERATION);
        let iseed = csum::inode_seed(seed, ino, generation);
        assert_eq!(
            csum::inode(iseed, raw),
            csum::stored_inode(raw),
            "inode {}",
            ino
        );
        // First directory block: a leaf with a checksum tail, or an
        // htree root.
        let b = img.block(img.bmap(raw, 0));
        if csum::has_dirent_tail(b) {
            let t = b.len() - 4;
            assert_eq!(
                csum::dirent_tail(iseed, b),
                le32(b, t),
                "dir block of inode {}",
                ino
            );
        }
    }
}

fn journal_replay(name: &str, open: &str) {
    let payload: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    let pfile = std::env::temp_dir().join(format!("ext4core-{}-payload", name));
    std::fs::write(&pfile, &payload).unwrap();
    let cmds = format!(
        "{}\njw -b 3000 {p}\njw -b 3001 {p}\njw -r 3001\njc\n",
        open,
        p = pfile.display()
    );
    let Some(img) = Image::make(name, &[], Some(&cmds)) else {
        return;
    };
    let _ = std::fs::remove_file(&pfile);
    let sb = img.sb();
    assert!(le32(sb, 0x60) & 0x4 != 0, "needs_recovery should be set");
    let jraw = img.inode(le32(sb, 0xE0));
    let jblock = |l: u32| img.block(img.bmap(jraw, l)).to_vec();
    let js = jbd2::Super::parse(&jblock(0)).unwrap();
    assert_ne!(js.start, 0);
    let r = jbd2::plan_replay(&js, |l| (l < js.maxlen).then(|| jblock(l)));
    assert!(r.transactions >= 1, "{:?}", r);
    // Block 3001 was revoked in the same transaction.
    let targets: Vec<u64> = r.writes.iter().map(|w| w.target).collect();
    assert_eq!(targets, [3000], "{:?}", r);
    let mut d = jblock(r.writes[0].log_block);
    jbd2::unescape(&mut d, &r.writes[0]);
    assert_eq!(&d[..], &payload[..]);
}

#[test]
fn replay_debugfs_journal() {
    journal_replay("plain", "jo");
}

#[test]
fn replay_debugfs_journal_with_checksums() {
    journal_replay("csum3", "jo -c");
}
