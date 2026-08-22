//! A project on disk.
//!
//! A project is a folder, so it can be sent to a friend in one piece:
//!
//! ```text
//! MySong.beat/
//!   project.json
//!   samples/
//!     kick.wav
//!     clap.wav
//! ```
//!
//! Sample paths in `project.json` are relative to the folder. A sample is copied in the
//! moment its track is added and deleted when the last track using it goes, rather than
//! being gathered up at save time. That way the folder is always complete — there is no
//! window where a project refers to a file somewhere else that could move or be deleted —
//! and the only way to break a project is to go into the folder and break it by hand.
//!
//! Nothing here is fast, and nothing here goes anywhere near the audio thread.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::model::{Project, PROJECT_VERSION};

/// What a project folder is called, so the file picker can filter for it.
pub const PROJECT_EXTENSION: &str = "beat";

/// The file the project itself lives in.
pub const PROJECT_FILE: &str = "project.json";

/// The folder samples are copied into, inside the project folder.
pub const SAMPLES_DIR: &str = "samples";

pub fn project_file(dir: &Path) -> PathBuf {
    dir.join(PROJECT_FILE)
}

pub fn samples_dir(dir: &Path) -> PathBuf {
    dir.join(SAMPLES_DIR)
}

/// True if this folder holds a project we could open.
pub fn is_project(dir: &Path) -> bool {
    project_file(dir).is_file()
}

/// What to call the project in the window: the folder name without the extension.
pub fn name_of(dir: &Path) -> String {
    dir.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

/// Turn a path out of `project.json` into a real one, refusing anything that points
/// outside the project folder. A hand-edited file is not a reason to write elsewhere.
pub fn resolve(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    let safe = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    if !safe || relative.as_os_str().is_empty() {
        return Err(format!("{} is not inside the project", relative.display()));
    }
    Ok(dir.join(relative))
}

/// Write `project.json`. Goes to a temporary file first and is renamed into place, so a
/// crash halfway through cannot leave half a project behind.
pub fn save(dir: &Path, project: &Project) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| whined(dir, "make", e))?;
    let json = serde_json::to_string_pretty(project)
        .map_err(|e| format!("could not write the project out: {e}"))?;
    let temp = dir.join("project.json.writing");
    fs::write(&temp, json).map_err(|e| whined(&temp, "write", e))?;
    fs::rename(&temp, project_file(dir)).map_err(|e| whined(dir, "save into", e))
}

/// Read `project.json`.
pub fn load(dir: &Path) -> Result<Project, String> {
    let path = project_file(dir);
    let text = fs::read_to_string(&path).map_err(|e| whined(&path, "read", e))?;
    let mut project: Project = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a Weetbeats project: {e}", path.display()))?;
    if project.version > PROJECT_VERSION {
        return Err(format!(
            "{} was made by a newer Weetbeats (version {}), so this one will not open it",
            name_of(dir),
            project.version
        ));
    }
    if project.patterns.is_empty() {
        return Err(format!("{} has no patterns in it", name_of(dir)));
    }
    // Every project gets looked over on the way in, so a file written by an older version
    // cannot leave the app holding something it has no way to edit.
    project.repair();
    Ok(project)
}

/// A sample that is now in the project folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    /// Where it is, relative to the project folder.
    pub path: String,
    /// True when the file was already there, byte for byte. The caller can then keep using
    /// whatever it decoded last time; a fresh copy means anything it remembers is stale.
    pub reused: bool,
}

/// Copy a sample into the project and give back its path relative to the folder.
///
/// A file already in there with the same name and the same contents is reused, so adding
/// the same kick twice does not leave two copies of it. One with the same name and
/// different contents gets a number, because two different kicks are both worth keeping.
pub fn import_sample(dir: &Path, source: &Path) -> Result<Imported, String> {
    let samples = samples_dir(dir);
    fs::create_dir_all(&samples).map_err(|e| whined(&samples, "make", e))?;

    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", source.display()))?;

    let mut candidate = samples.join(name);
    let mut attempt = 2;
    while candidate.exists() {
        if same_file_contents(source, &candidate) {
            // Already in the project, and identical. Nothing to copy.
            return Ok(Imported {
                path: relative(name_of_file(&candidate)),
                reused: true,
            });
        }
        candidate = samples.join(numbered(name, attempt));
        attempt += 1;
        if attempt > 999 {
            return Err(format!("too many samples called {name}"));
        }
    }

    fs::copy(source, &candidate).map_err(|e| whined(&candidate, "copy into", e))?;
    Ok(Imported {
        path: relative(name_of_file(&candidate)),
        reused: false,
    })
}

/// Delete a sample from the project. Call it when the last track using it has gone.
/// Missing is fine: the point is that the file is not there afterwards.
pub fn remove_sample(dir: &Path, path: &str) -> Result<(), String> {
    let full = resolve(dir, path)?;
    if !full.starts_with(samples_dir(dir)) {
        return Err(format!("{path} is not one of the project's samples"));
    }
    match fs::remove_file(&full) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(whined(&full, "delete", e)),
    }
}

/// Copy a whole project folder somewhere else, for "save as". The destination is made if
/// it is not there, and files already in it with the same names are overwritten.
pub fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    fs::create_dir_all(to).map_err(|e| whined(to, "make", e))?;
    let samples = samples_dir(from);
    if samples.is_dir() {
        let into = samples_dir(to);
        fs::create_dir_all(&into).map_err(|e| whined(&into, "make", e))?;
        for entry in fs::read_dir(&samples).map_err(|e| whined(&samples, "read", e))? {
            let entry = entry.map_err(|e| whined(&samples, "read", e))?;
            if entry.path().is_file() {
                let target = into.join(entry.file_name());
                fs::copy(entry.path(), &target).map_err(|e| whined(&target, "copy into", e))?;
            }
        }
    }
    let project = project_file(from);
    if project.is_file() {
        fs::copy(&project, project_file(to)).map_err(|e| whined(to, "copy into", e))?;
    }
    Ok(())
}

/// Delete samples in the folder that no track in the project refers to.
///
/// Nothing normally leaves anything behind — a sample goes when its track does — so this
/// is for the folder someone has been editing by hand, and for a project saved by a
/// version that did not tidy up. Returns how many files went.
pub fn forget_unused_samples(dir: &Path, project: &Project) -> Result<usize, String> {
    let samples = samples_dir(dir);
    if !samples.is_dir() {
        return Ok(0);
    }
    let mut gone = 0;
    for entry in fs::read_dir(&samples).map_err(|e| whined(&samples, "read", e))? {
        let entry = entry.map_err(|e| whined(&samples, "read", e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let used = project.tracks.iter().any(|t| {
            t.sample
                .as_ref()
                .and_then(|s| resolve(dir, &s.path).ok())
                .is_some_and(|p| p == path)
        });
        if !used {
            fs::remove_file(&path).map_err(|e| whined(&path, "delete", e))?;
            gone += 1;
        }
    }
    Ok(gone)
}

fn relative(name: &str) -> String {
    format!("{SAMPLES_DIR}/{name}")
}

fn name_of_file(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
}

/// `kick.wav` and 2 becomes `kick 2.wav`.
fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} {n}.{ext}"),
        _ => format!("{name} {n}"),
    }
}

/// Same length and the same bytes. Cheap enough: samples are small, and being wrong here
/// means either a duplicate file or, worse, the wrong sound.
fn same_file_contents(a: &Path, b: &Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if a_meta.len() != b_meta.len() {
        return false;
    }
    match (fs::read(a), fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn whined(path: &Path, doing: &str, e: io::Error) -> String {
    format!("could not {doing} {}: {e}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SampleRef, Track};

    /// A folder under the system temp dir that cleans up after itself.
    struct Temp(PathBuf);

    impl Temp {
        fn new(what: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("weetbeats-{what}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_project_survives_the_round_trip_to_disk() {
        let temp = Temp::new("save");
        let dir = temp.path().join("MySong.beat");

        let mut project = Project {
            bpm: 143.0,
            ..Default::default()
        };
        let second = project.add_pattern().unwrap();
        project.set_pattern_steps(second, 32);
        project.set_placement(second, 0, true);

        save(&dir, &project).unwrap();
        assert!(is_project(&dir));
        assert_eq!(name_of(&dir), "MySong");

        let back = load(&dir).unwrap();
        assert_eq!(back.bpm, 143.0);
        assert_eq!(back.pattern(second).unwrap().steps, 32);
        assert!(back.placed(second, 0));
    }

    #[test]
    fn a_half_written_project_is_never_left_behind() {
        let temp = Temp::new("atomic");
        let dir = temp.path().join("MySong.beat");
        save(&dir, &Project::default()).unwrap();
        // The temporary file is renamed into place, not left lying around.
        assert!(!dir.join("project.json.writing").exists());
        assert!(load(&dir).is_ok());
    }

    /// Opening a project leaves the music where it is. An older version could write a
    /// placement that no click could land on, and the answer to that is hit testing the
    /// block rather than shoving it onto a grid it was never on.
    #[test]
    fn opening_a_project_does_not_move_anything() {
        let temp = Temp::new("repair");
        let dir = temp.path().join("Wonky.beat");

        let mut project = Project::default();
        project.set_pattern_steps(0, 32);
        project.set_placement(0, 0, true);
        // Where a length change under an older version would have left it: on the half bar.
        project.song[0].step = 48;
        save(&dir, &project).unwrap();

        let back = load(&dir).unwrap();
        assert!(back.placed(0, 48), "it moved the music");
    }

    /// A placement of a pattern that is not there is the one thing worth throwing away.
    #[test]
    fn a_placement_of_a_pattern_that_is_gone_is_dropped() {
        let temp = Temp::new("orphan");
        let dir = temp.path().join("Orphan.beat");

        let mut project = Project::default();
        project.set_placement(0, 0, true);
        project.song.push(crate::model::Placement {
            step: 32,
            pattern: 9,
        });
        save(&dir, &project).unwrap();

        let back = load(&dir).unwrap();
        assert_eq!(back.song.len(), 1);
        assert!(back.placed(0, 0));
    }

    #[test]
    fn a_project_from_the_future_says_so_rather_than_guessing() {
        let temp = Temp::new("version");
        let dir = temp.path().join("Later.beat");
        let project = Project {
            version: PROJECT_VERSION + 5,
            ..Default::default()
        };
        save(&dir, &project).unwrap();

        let e = load(&dir).unwrap_err();
        assert!(e.contains("newer Weetbeats"), "{e}");
    }

    #[test]
    fn a_sample_is_copied_in_and_referred_to_relatively() {
        let temp = Temp::new("import");
        let dir = temp.path().join("Song.beat");
        let source = temp.file("kick.wav", b"boom");

        let sample = import_sample(&dir, &source).unwrap();
        assert_eq!(sample.path, "samples/kick.wav");
        assert!(!sample.reused);
        assert_eq!(
            fs::read(resolve(&dir, &sample.path).unwrap()).unwrap(),
            b"boom"
        );

        // The project keeps working when the original goes away, which is the whole point.
        fs::remove_file(&source).unwrap();
        assert!(resolve(&dir, &sample.path).unwrap().is_file());
    }

    #[test]
    fn the_same_sample_twice_is_one_file() {
        let temp = Temp::new("same");
        let dir = temp.path().join("Song.beat");
        let source = temp.file("kick.wav", b"boom");

        let first = import_sample(&dir, &source).unwrap();
        let second = import_sample(&dir, &source).unwrap();
        assert_eq!(first.path, second.path);
        assert!(!first.reused);
        assert!(
            second.reused,
            "the second copy was not spotted as the same file"
        );
        assert_eq!(fs::read_dir(samples_dir(&dir)).unwrap().count(), 1);
    }

    #[test]
    fn two_different_samples_with_one_name_both_get_kept() {
        let temp = Temp::new("clash");
        let dir = temp.path().join("Song.beat");
        let mine = temp.file("kick.wav", b"boom");
        fs::create_dir_all(temp.path().join("elsewhere")).unwrap();
        let theirs = temp.file("elsewhere/kick.wav", b"different boom");

        assert_eq!(import_sample(&dir, &mine).unwrap().path, "samples/kick.wav");
        let theirs = import_sample(&dir, &theirs).unwrap();
        assert_eq!(theirs.path, "samples/kick 2.wav");
        assert!(
            !theirs.reused,
            "a different sound must not be taken for the same one"
        );
        assert_eq!(
            fs::read(resolve(&dir, "samples/kick 2.wav").unwrap()).unwrap(),
            b"different boom"
        );
    }

    #[test]
    fn a_sample_goes_when_its_track_does() {
        let temp = Temp::new("remove");
        let dir = temp.path().join("Song.beat");
        let source = temp.file("clap.wav", b"clap");
        let path = import_sample(&dir, &source).unwrap().path;

        remove_sample(&dir, &path).unwrap();
        assert!(!resolve(&dir, &path).unwrap().exists());
        // Asking twice is not an error: what matters is that it is gone.
        remove_sample(&dir, &path).unwrap();
    }

    #[test]
    fn a_hand_edited_path_cannot_reach_out_of_the_project() {
        let temp = Temp::new("escape");
        let dir = temp.path().join("Song.beat");
        let outside = temp.file("precious.wav", b"do not touch");

        assert!(resolve(&dir, "../precious.wav").is_err());
        assert!(remove_sample(&dir, "../precious.wav").is_err());
        // An absolute path is not relative to anything, so it is refused too.
        assert!(remove_sample(&dir, outside.to_str().unwrap()).is_err());
        assert!(outside.is_file(), "it deleted a file outside the project");
    }

    #[test]
    fn save_as_takes_the_samples_with_it() {
        let temp = Temp::new("saveas");
        let from = temp.path().join("First.beat");
        let to = temp.path().join("Second.beat");
        let source = temp.file("hat.wav", b"tss");

        let path = import_sample(&from, &source).unwrap().path;
        let mut project = Project::default();
        project.tracks.push(Track::new(
            0,
            "hat".into(),
            Some(SampleRef {
                path: path.clone(),
                name: "hat".into(),
            }),
        ));
        save(&from, &project).unwrap();

        copy_folder(&from, &to).unwrap();
        assert!(is_project(&to));
        assert_eq!(fs::read(resolve(&to, &path).unwrap()).unwrap(), b"tss");
        assert_eq!(load(&to).unwrap().tracks.len(), 1);
    }

    #[test]
    fn samples_nothing_refers_to_get_tidied_away() {
        let temp = Temp::new("tidy");
        let dir = temp.path().join("Song.beat");
        let kept = import_sample(&dir, &temp.file("kick.wav", b"boom"))
            .unwrap()
            .path;
        import_sample(&dir, &temp.file("stray.wav", b"nobody wants me")).unwrap();

        let mut project = Project::default();
        project.tracks.push(Track::new(
            0,
            "kick".into(),
            Some(SampleRef {
                path: kept.clone(),
                name: "kick".into(),
            }),
        ));

        assert_eq!(forget_unused_samples(&dir, &project).unwrap(), 1);
        assert!(resolve(&dir, &kept).unwrap().is_file());
        assert!(!resolve(&dir, "samples/stray.wav").unwrap().exists());
    }
}
