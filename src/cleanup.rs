//! Deleting a profile, truthfully.
//!
//! Deleting removes: both WebKit data stores (the profile's and its Search
//! store), the sealed session backups, the saved login, every tab's saved
//! page state, the profile's PDF previews and its per-site zoom levels. Each
//! of those is a step of a job written to `cleanup.json` *before* anything is
//! removed, so a crash or a failure leaves a job that is retried at the next
//! launch. A job is only finished once every step is verified gone; until
//! then the UI says what is left and offers Retry. Nothing claims
//! "permanently removed" before that.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::util;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase", tag = "step", content = "id")]
pub enum Step {
    WebsiteData(Uuid),
    SessionBackup(Uuid),
    SavedLogin,
    TabState(Uuid),
    Preview(String),
    SiteSettings,
}

impl Step {
    pub fn describe(&self) -> &'static str {
        match self {
            Step::WebsiteData(_) => "website data (cookies, storage, cache)",
            Step::SessionBackup(_) => "saved session",
            Step::SavedLogin => "saved login",
            Step::TabState(_) => "saved tab state",
            Step::Preview(_) => "PDF previews",
            Step::SiteSettings => "site settings",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub client: Uuid,
    pub name: String,
    /// Steps not yet verified done.
    pub pending: Vec<Step>,
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Job {
    pub fn new(client: Uuid, name: &str, stores: &[Uuid], tabs: &[Uuid], previews: &[String]) -> Job {
        let mut pending = vec![Step::SavedLogin, Step::SiteSettings];
        for s in stores {
            pending.push(Step::SessionBackup(*s));
            pending.push(Step::WebsiteData(*s));
        }
        pending.extend(tabs.iter().map(|t| Step::TabState(*t)));
        pending.extend(previews.iter().cloned().map(Step::Preview));
        Job { client, name: name.to_string(), pending, attempts: 0, last_error: None }
    }

    /// What is still left, for the user, without duplicates.
    pub fn remaining(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = vec![];
        for s in &self.pending {
            if !v.contains(&s.describe()) {
                v.push(s.describe());
            }
        }
        v
    }
}

/// The removal primitives, so tests can inject failures. Each returns Ok
/// only once the thing is verified gone.
pub trait Cleaner {
    fn saved_login(&self, client: Uuid) -> Result<(), String>;
    fn session_backup(&self, store: Uuid) -> Result<(), String>;
    fn tab_state(&self, tab: Uuid) -> Result<(), String>;
    fn preview(&self, path: &str) -> Result<(), String>;
    fn site_settings(&self, client: Uuid) -> Result<(), String>;
}

/// Run every synchronous step; verified steps leave `pending`. Website data
/// is left for the asynchronous WebKit pass.
pub fn run_local_steps(job: &mut Job, c: &dyn Cleaner) {
    job.attempts += 1;
    let mut errors = vec![];
    let client = job.client;
    job.pending.retain(|step| {
        let r = match step {
            Step::WebsiteData(_) => return true,
            Step::SavedLogin => c.saved_login(client),
            Step::SessionBackup(s) => c.session_backup(*s),
            Step::TabState(t) => c.tab_state(*t),
            Step::Preview(p) => c.preview(p),
            Step::SiteSettings => c.site_settings(client),
        };
        match r {
            Ok(()) => false,
            Err(e) => {
                errors.push(format!("{}: {e}", step.describe()));
                true
            }
        }
    });
    job.last_error = (!errors.is_empty()).then(|| errors.join("; "));
}

// ---- persistence ------------------------------------------------------------

fn path() -> PathBuf {
    util::support_dir().join("cleanup.json")
}

thread_local! {
    static JOBS: RefCell<Option<Vec<Job>>> = const { RefCell::new(None) };
}

fn load() -> Vec<Job> {
    match std::fs::read(path()) {
        Ok(d) => serde_json::from_slice(&d).unwrap_or_else(|e| {
            // Never drop unfinished work silently: keep the file aside.
            let _ = std::fs::rename(path(), path().with_extension(format!("unreadable-{}.json", std::process::id())));
            util::log(format!("cleanup list unreadable ({e}); kept aside"));
            vec![]
        }),
        Err(_) => vec![],
    }
}

fn with_jobs<R>(f: impl FnOnce(&mut Vec<Job>) -> R) -> R {
    JOBS.with_borrow_mut(|j| f(j.get_or_insert_with(load)))
}

/// Written synchronously: the job must be on disk before anything is
/// removed.
fn persist() {
    let data = with_jobs(|j| serde_json::to_vec_pretty(j).unwrap_or_default());
    let r = if data == b"[]" {
        std::fs::remove_file(path()).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
    } else {
        util::write_atomic(&path(), &data, 0o600)
    };
    if let Err(e) = r {
        util::log(format!("could not save the cleanup list: {e}"));
    }
}

pub fn pending_jobs() -> Vec<Job> {
    with_jobs(|j| j.clone())
}

// ---- the real cleaner -----------------------------------------------------

struct Disk;

impl Cleaner for Disk {
    fn saved_login(&self, client: Uuid) -> Result<(), String> {
        if crate::keychain::delete_login(client) && !crate::keychain::login_exists(client) { Ok(()) } else { Err("still present".into()) }
    }

    fn session_backup(&self, store: Uuid) -> Result<(), String> {
        crate::session::forget(store);
        util::flush_writes();
        if crate::session::backup_gone(store) { Ok(()) } else { Err("still present".into()) }
    }

    fn tab_state(&self, tab: Uuid) -> Result<(), String> {
        crate::tabstate::delete(tab);
        util::flush_writes();
        if crate::tabstate::exists(tab) { Err("still present".into()) } else { Ok(()) }
    }

    fn preview(&self, path: &str) -> Result<(), String> {
        let root = util::pdf_previews_dir();
        let p = std::path::Path::new(path);
        crate::preview::remove_preview(&root, p);
        if p.exists() { Err("still present".into()) } else { Ok(()) }
    }

    fn site_settings(&self, client: Uuid) -> Result<(), String> {
        crate::settings::forget_profile(client);
        Ok(())
    }
}

/// Start deleting a profile's data. The profile is already gone from the
/// list; this removes what it left behind.
pub fn start(job: Job) {
    with_jobs(|j| {
        j.retain(|x| x.client != job.client);
        j.push(job.clone());
    });
    persist();
    run(job.client);
}

/// Retry every unfinished job (at launch, and from Retry).
pub fn resume_all() {
    for id in with_jobs(|j| j.iter().map(|x| x.client).collect::<Vec<_>>()) {
        run(id);
    }
}

pub fn run(client: Uuid) {
    let Some(mut job) = with_jobs(|j| j.iter().find(|x| x.client == client).cloned()) else { return };
    run_local_steps(&mut job, &Disk);
    let stores: Vec<Uuid> = job.pending.iter().filter_map(|s| if let Step::WebsiteData(id) = s { Some(*id) } else { None }).collect();
    save_job(job);
    if stores.is_empty() {
        return finish_if_done(client);
    }
    // WebKit removes a store only once no web view uses it.
    util::after(0.5, move || {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(stores.len()));
        for id in stores {
            let remaining = remaining.clone();
            crate::web::remove_data_store(id, move |result| {
                with_jobs(|j| {
                    if let Some(job) = j.iter_mut().find(|x| x.client == client) {
                        match &result {
                            Ok(()) => job.pending.retain(|s| *s != Step::WebsiteData(id)),
                            Err(e) => job.last_error = Some(format!("website data: {e}")),
                        }
                    }
                });
                persist();
                remaining.set(remaining.get() - 1);
                if remaining.get() == 0 {
                    finish_if_done(client);
                }
            });
        }
    });
}

fn save_job(job: Job) {
    with_jobs(|j| {
        if let Some(slot) = j.iter_mut().find(|x| x.client == job.client) {
            *slot = job;
        }
    });
    persist();
}

fn finish_if_done(client: Uuid) {
    let job = with_jobs(|j| j.iter().find(|x| x.client == client).cloned());
    let Some(job) = job else { return };
    if job.pending.is_empty() {
        with_jobs(|j| j.retain(|x| x.client != client));
        persist();
        util::log("profile data removed and verified");
    } else {
        util::log(format!("profile cleanup incomplete after {} attempt(s): {}", job.attempts, job.remaining().join(", ")));
    }
    crate::app::cleanup_finished(job);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Fake {
        fail_login: Cell<bool>,
        fail_previews: Cell<bool>,
    }

    impl Cleaner for Fake {
        fn saved_login(&self, _: Uuid) -> Result<(), String> {
            if self.fail_login.get() { Err("permission denied".into()) } else { Ok(()) }
        }
        fn session_backup(&self, _: Uuid) -> Result<(), String> {
            Ok(())
        }
        fn tab_state(&self, _: Uuid) -> Result<(), String> {
            Ok(())
        }
        fn preview(&self, _: &str) -> Result<(), String> {
            if self.fail_previews.get() { Err("busy".into()) } else { Ok(()) }
        }
        fn site_settings(&self, _: Uuid) -> Result<(), String> {
            Ok(())
        }
    }

    fn job() -> Job {
        Job::new(Uuid::new_v4(), "Acme", &[Uuid::new_v4(), Uuid::new_v4()], &[Uuid::new_v4()], &["/p/a/x.pdf".into()])
    }

    #[test]
    fn a_job_lists_every_kind_of_data() {
        let j = job();
        let r = j.remaining();
        for what in ["saved login", "saved session", "website data (cookies, storage, cache)", "saved tab state", "PDF previews", "site settings"] {
            assert!(r.contains(&what), "{what}");
        }
    }

    #[test]
    fn failures_stay_pending_and_retry_finishes_them() {
        let fake = Fake::default();
        fake.fail_login.set(true);
        fake.fail_previews.set(true);
        let mut j = job();
        run_local_steps(&mut j, &fake);
        assert!(j.pending.contains(&Step::SavedLogin));
        assert!(j.pending.iter().any(|s| matches!(s, Step::Preview(_))));
        assert!(j.last_error.as_deref().unwrap().contains("saved login: permission denied"));
        assert_eq!(j.pending.iter().filter(|s| matches!(s, Step::WebsiteData(_))).count(), 2, "WebKit work is separate");
        fake.fail_login.set(false);
        fake.fail_previews.set(false);
        run_local_steps(&mut j, &fake);
        assert_eq!(j.attempts, 2);
        assert!(j.last_error.is_none());
        assert!(j.pending.iter().all(|s| matches!(s, Step::WebsiteData(_))));
    }

    #[test]
    fn jobs_survive_a_restart() {
        let j = job();
        let json = serde_json::to_vec(&vec![j.clone()]).unwrap();
        let back: Vec<Job> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, vec![j]);
    }
}
