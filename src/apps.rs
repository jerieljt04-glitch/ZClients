//! The Zoho app catalogue behind the new-tab picker, and the forgiving
//! search that filters it as the user types.
//!
//! Addresses were checked against Zoho's servers: each `https://<slug>.zoho.com`
//! answers (and, once signed in, opens the app). Catalyst, OneAuth and
//! Log360 Cloud are the exceptions and have their own addresses.

pub struct ZohoApp {
    /// Display name without the "Zoho " prefix, e.g. "CRM".
    pub name: &'static str,
    /// The host label that identifies the app, e.g. "crm" in crm.zoho.com.
    pub slug: &'static str,
    pub url: &'static str,
    pub category: &'static str,
    /// Plain words people search with ("invoice", "chat", "passwords").
    pub keywords: &'static str,
    pub color: &'static str,
}

impl ZohoApp {
    pub fn full_name(&self) -> String {
        if self.name.starts_with("Zoho") { self.name.to_string() } else { format!("Zoho {}", self.name) }
    }

    pub fn initials(&self) -> String {
        let mut chars = self.name.chars().filter(|c| c.is_alphanumeric());
        let first = chars.next().map(|c| c.to_ascii_uppercase()).unwrap_or('Z');
        // Second letter: the next capital if the name has one (WorkDrive ->
        // WD, SalesIQ -> SI), otherwise the next letter.
        let second = self.name.chars().skip(1).find(|c| c.is_ascii_uppercase()).or_else(|| self.name.chars().nth(1)).unwrap_or(' ');
        format!("{first}{}", second.to_ascii_lowercase()).trim().to_string()
    }
}

const SALES: &str = "Sales & Customer Engagement";
const MARKETING: &str = "Marketing";
const FINANCE: &str = "Finance & Operations";
const COLLAB: &str = "Communication & Collaboration";
const OFFICE: &str = "Office Productivity";
const HR: &str = "Human Resources";
const SUPPORT: &str = "Customer Support & Field Service";
const PLATFORM: &str = "Custom Platform, Data & Developer Tools";
const SECURITY: &str = "Security & Governance";
const SUITE: &str = "Suite";

macro_rules! app {
    ($name:expr, $slug:expr, $cat:expr, $color:expr, $kw:expr) => {
        ZohoApp { name: $name, slug: $slug, url: concat!("https://", $slug, ".zoho.com"), category: $cat, keywords: $kw, color: $color }
    };
    ($name:expr, $slug:expr, $url:expr, $cat:expr, $color:expr, $kw:expr) => {
        ZohoApp { name: $name, slug: $slug, url: $url, category: $cat, keywords: $kw, color: $color }
    };
}

pub const ZOHO_APPS: &[ZohoApp] = &[
    app!("CRM", "crm", SALES, "#e42527", "sales leads deals contacts accounts customers pipeline"),
    app!("Bigin", "bigin", SALES, "#f0483e", "pipeline small business crm deals"),
    app!("Bookings", "bookings", SALES, "#ff7a45", "appointments scheduling calendar booking meetings"),
    app!("CommandCenter", "commandcenter", SALES, "#c2410c", "customer journeys orchestration automation"),
    app!("TouchPoint", "touchpoint", SALES, "#e8563f", "customer engagement touchpoints"),
    app!("Campaigns", "campaigns", MARKETING, "#d64d8a", "email marketing newsletter mailing list"),
    app!("Marketing Automation", "marketingautomation", MARKETING, "#db2777", "journeys lead scoring nurture marketing"),
    app!("Social", "social", MARKETING, "#e1306c", "social media posts facebook instagram linkedin twitter x"),
    app!("SalesIQ", "salesiq", MARKETING, "#f97316", "live chat website visitors chatbot"),
    app!("Forms", "forms", MARKETING, "#22a447", "form builder online forms"),
    app!("Survey", "survey", MARKETING, "#0ea5a4", "questionnaire feedback poll"),
    app!("PageSense", "pagesense", MARKETING, "#7c3aed", "ab testing heatmaps conversion optimisation"),
    app!("Backstage", "backstage", MARKETING, "#9333ea", "events conference webinar tickets"),
    app!("LandingPage", "landingpage", MARKETING, "#2563eb", "landing page builder"),
    app!("Sites", "sites", MARKETING, "#0891b2", "website builder web site"),
    app!("Publish", "publish", MARKETING, "#4f46e5", "business listings local seo"),
    app!("Thrive", "thrive", MARKETING, "#16a34a", "affiliate loyalty referral"),
    app!("Linkthread", "linkthread", MARKETING, "#be123c", "link in bio links"),
    app!("Books", "books", FINANCE, "#22a447", "accounting bookkeeping ledger finance invoices"),
    app!("Invoice", "invoice", FINANCE, "#2b7de9", "invoicing invoices quotes estimates billing"),
    app!("Expense", "expense", FINANCE, "#0e9488", "expenses reimbursement receipts travel claims"),
    app!("Inventory", "inventory", FINANCE, "#0e7490", "stock warehouse orders items"),
    app!("Billing", "billing", FINANCE, "#15803d", "subscriptions recurring billing"),
    app!("Payroll", "payroll", FINANCE, "#65a30d", "salary payslips wages"),
    app!("Checkout", "checkout", FINANCE, "#a16207", "payments payment pages collect"),
    app!("Commerce", "commerce", FINANCE, "#ea580c", "ecommerce online store shop"),
    app!("Spend", "spend", FINANCE, "#b45309", "spend management corporate cards"),
    app!("Procurement", "procurement", FINANCE, "#78350f", "purchasing vendors purchase orders suppliers"),
    app!("Practice", "practice", FINANCE, "#475569", "accounting firm clients practice management"),
    app!("Solo", "solo", FINANCE, "#334155", "freelancer self employed"),
    app!("Mail", "mail", COLLAB, "#2b7de9", "email inbox"),
    app!("Cliq", "cliq", COLLAB, "#2fb457", "chat messaging team chat channels"),
    app!("Projects", "projects", COLLAB, "#f2a516", "tasks project management gantt timesheets"),
    app!("Sprints", "sprints", COLLAB, "#d97706", "agile scrum backlog jira"),
    app!("WorkDrive", "workdrive", COLLAB, "#3d63d6", "files drive storage documents folders"),
    app!("Meeting", "meeting", COLLAB, "#1d4ed8", "video calls webinar conference"),
    app!("Connect", "connect", COLLAB, "#6366f1", "intranet social network team"),
    app!("TeamInbox", "teaminbox", COLLAB, "#0284c7", "shared inbox team email"),
    app!("Learn", "learn", COLLAB, "#7a5cf0", "knowledge base training courses lms"),
    app!("Vani", "vani", COLLAB, "#c026d3", "whiteboard visual collaboration diagrams mind map"),
    app!("Writer", "writer", OFFICE, "#2563eb", "documents word processor docs"),
    app!("Sheet", "sheet", OFFICE, "#16a34a", "spreadsheet excel"),
    app!("Show", "show", OFFICE, "#ea580c", "presentation slides powerpoint"),
    app!("Sign", "sign", OFFICE, "#c2410c", "esignature signatures sign documents"),
    app!("Notebook", "notebook", OFFICE, "#f59e0b", "notes notebooks"),
    app!("Vault", "vault", OFFICE, "#1e293b", "passwords password manager"),
    app!("Tables", "tables", OFFICE, "#0ea5a4", "database airtable tables"),
    app!("ToDo", "todo", OFFICE, "#2563eb", "tasks to-do checklist"),
    app!("People", "people", HR, "#7a5cf0", "hr employees attendance leave time off"),
    app!("Recruit", "recruit", HR, "#9333ea", "hiring ats candidates jobs recruitment"),
    app!("Workerly", "workerly", HR, "#6d28d9", "temporary staffing temp workers"),
    app!("Shifts", "shifts", HR, "#4338ca", "scheduling rota shifts staff schedule"),
    app!("Desk", "desk", SUPPORT, "#00a3b3", "helpdesk tickets support customer service"),
    app!("Assist", "assist", SUPPORT, "#0891b2", "remote support screen sharing remote access"),
    app!("Lens", "lens", SUPPORT, "#0e7490", "augmented reality remote assistance camera"),
    app!("FSM", "fsm", SUPPORT, "#0f766e", "field service management work orders technicians"),
    app!("Voice", "voice", SUPPORT, "#155e75", "phone telephony calls call centre"),
    app!("Creator", "creator", PLATFORM, "#1fa2a6", "low code custom apps builder"),
    app!("Analytics", "analytics", PLATFORM, "#e8563f", "reports dashboards bi business intelligence"),
    app!("Flow", "flow", PLATFORM, "#2563eb", "integrations automation workflows zapier"),
    app!("DataPrep", "dataprep", PLATFORM, "#0d9488", "data cleaning preparation etl"),
    app!("Catalyst", "catalyst", "https://console.catalyst.zoho.com", PLATFORM, "#7c3aed", "serverless functions developer cloud"),
    app!("RPA", "rpa", PLATFORM, "#475569", "robotic process automation bots"),
    app!("Directory", "directory", SECURITY, "#334155", "users sso identity admin directory"),
    app!("OneAuth", "oneauth", "https://accounts.zoho.com/home", SECURITY, "#1e3a8a", "2fa mfa authenticator two factor security"),
    app!("Log360 Cloud", "log360cloud", "https://log360cloud.manageengine.com", SECURITY, "#7f1d1d", "siem logs security monitoring audit"),
    app!("Contracts", "contracts", SECURITY, "#44291a", "contract management clm agreements"),
    app!("Zoho One", "one", SUITE, "#e42527", "suite all apps admin panel"),
];

/// The app a page belongs to, from its host: crm.zoho.com, crm.zoho.eu,
/// console.catalyst.zoho.com and log360cloud.manageengine.com all resolve.
pub fn app_for_host(host: &str) -> Option<&'static ZohoApp> {
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let zoho_at = labels.iter().position(|l| *l == "zoho" || *l == "manageengine")?;
    ZOHO_APPS.iter().find(|a| labels[..zoho_at].contains(&a.slug))
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Restricted Damerau–Levenshtein distance, for typo tolerance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// Letters of `q` appear in order in `s` ("wrkdrv" in "workdrive").
fn is_subsequence(q: &str, s: &str) -> bool {
    let mut it = s.chars();
    q.chars().all(|c| it.any(|x| x == c))
}

/// How well one query word matches one app (higher is better).
fn word_score(word: &str, app: &ZohoApp) -> Option<i32> {
    let name = norm(app.name);
    let compact = name.replace(' ', "");
    let name_words: Vec<&str> = name.split(' ').collect();
    let keywords = norm(app.keywords);
    let category = norm(app.category);
    if compact == word || name == word {
        return Some(1000);
    }
    if compact.starts_with(word) {
        return Some(900 - (compact.len() - word.len()) as i32);
    }
    if name_words.iter().any(|w| w.starts_with(word)) {
        return Some(800);
    }
    if compact.contains(word) {
        return Some(700);
    }
    if keywords.split(' ').any(|k| k == word) {
        return Some(650);
    }
    if keywords.split(' ').any(|k| k.starts_with(word)) && word.len() >= 2 {
        return Some(600);
    }
    if word.len() >= 3 && category.split(' ').any(|c| c.starts_with(word)) {
        return Some(450);
    }
    if word.len() >= 3 && is_subsequence(word, &compact) && word.chars().next() == compact.chars().next() {
        return Some(400);
    }
    // Typos: compare against the name (or its first part when the query is
    // shorter) and against each keyword.
    if word.len() >= 3 {
        let allowed = if word.len() >= 7 { 2 } else { 1 };
        let prefix: String = compact.chars().take(word.chars().count()).collect();
        let d = edit_distance(word, &compact).min(edit_distance(word, &prefix));
        if d <= allowed {
            return Some(350 - d as i32 * 20);
        }
        if word.len() >= 4 && keywords.split(' ').any(|k| edit_distance(word, k) <= allowed) {
            return Some(250);
        }
    }
    None
}

/// The apps most people mean when a query fits several equally well.
const POPULAR: &[&str] = &[
    "crm",
    "mail",
    "books",
    "desk",
    "projects",
    "workdrive",
    "cliq",
    "people",
    "analytics",
    "creator",
    "sign",
    "invoice",
    "campaigns",
    "inventory",
    "recruit",
    "writer",
    "sheet",
    "one",
];

/// Indexes into [`ZOHO_APPS`] matching `query`, best first. Every word must
/// match something; an empty query returns the whole catalogue in order.
pub fn search(query: &str) -> Vec<usize> {
    let q = norm(query);
    let q = q.strip_prefix("zoho ").unwrap_or(if q == "zoho" { "" } else { &q }).to_string();
    if q.is_empty() {
        return (0..ZOHO_APPS.len()).collect();
    }
    let words: Vec<&str> = q.split(' ').collect();
    let mut scored: Vec<(i32, usize)> = ZOHO_APPS
        .iter()
        .enumerate()
        .filter_map(|(i, app)| {
            // A multi-word name typed in full ("marketing automation").
            let whole = word_score(&q.replace(' ', ""), app).filter(|s| *s >= 700);
            let per_word: Option<i32> = words.iter().map(|w| word_score(w, app)).sum();
            let bonus = if POPULAR.contains(&app.slug) { 10 } else { 0 };
            whole.or(per_word.map(|s| s / words.len() as i32)).map(|s| (s + bonus, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(q: &str) -> &'static str {
        ZOHO_APPS[*search(q).first().unwrap_or_else(|| panic!("no result for {q:?}"))].name
    }

    #[test]
    fn exact_prefix_and_zoho_prefix() {
        assert_eq!(top("crm"), "CRM");
        assert_eq!(top("Zoho CRM"), "CRM");
        assert_eq!(top("boo"), "Books");
        assert_eq!(top("work"), "WorkDrive");
        assert_eq!(top("marketing automation"), "Marketing Automation");
        assert_eq!(search("zoho").len(), ZOHO_APPS.len());
    }

    #[test]
    fn plain_words_find_the_right_app() {
        assert_eq!(top("invoice"), "Invoice");
        assert_eq!(top("passwords"), "Vault");
        assert_eq!(top("chat"), "Cliq");
        assert_eq!(top("helpdesk"), "Desk");
        assert_eq!(top("spreadsheet"), "Sheet");
        assert!(search("accounting").iter().any(|i| ZOHO_APPS[*i].name == "Books"));
    }

    #[test]
    fn typos_and_abbreviations() {
        assert_eq!(top("anlytics"), "Analytics");
        assert_eq!(top("recuit"), "Recruit");
        assert_eq!(top("wrkdrv"), "WorkDrive");
        assert_eq!(top("campains"), "Campaigns");
    }

    #[test]
    fn nonsense_finds_nothing() {
        assert!(search("qqqzzz").is_empty());
    }

    #[test]
    fn hosts_map_back_to_apps() {
        assert_eq!(app_for_host("crm.zoho.eu").map(|a| a.name), Some("CRM"));
        assert_eq!(app_for_host("console.catalyst.zoho.com").map(|a| a.name), Some("Catalyst"));
        assert_eq!(app_for_host("log360cloud.manageengine.com").map(|a| a.name), Some("Log360 Cloud"));
        assert!(app_for_host("www.google.com").is_none());
    }

    #[test]
    fn catalogue_has_every_requested_app_once() {
        assert_eq!(ZOHO_APPS.len(), 68);
        let mut slugs: Vec<&str> = ZOHO_APPS.iter().map(|a| a.slug).collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), ZOHO_APPS.len());
        assert_eq!(ZOHO_APPS[0].initials(), "Cr");
        assert_eq!(ZOHO_APPS.iter().find(|a| a.name == "WorkDrive").unwrap().initials(), "Wd");
    }
}
