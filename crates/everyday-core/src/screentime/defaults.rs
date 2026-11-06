//! What a vault starts with, and what is known without being told: the
//! starter categories, which apps are browsers, friendlier names for common
//! app ids, and a suggested category for well-known apps and sites.
//!
//! Suggestions are never written into a label. They are applied at report
//! time to items nobody labelled, so improving this table improves every
//! day already recorded.
//!
//! # One app, many names
//!
//! The same program reaches here under a different id on every platform:
//! `org.gnome.Ptyxis` from a GNOME desktop file, `ptyxis` as an X11 class,
//! `Code.exe` on Windows, `com.microsoft.VSCode` on a Mac. Every table
//! below is keyed lower-case, and an id is looked up as it came and then
//! without a trailing `.exe` or `.desktop`, so `code.exe` and `code` find
//! the same row and the tables need not list both.

use super::ScreenCategory;

/// One starter category.
pub struct Starter {
    /// Stable key, kept in [`ScreenCategory::builtin`].
    pub builtin: &'static str,
    pub name: &'static str,
    pub color: &'static str,
}

/// The starter categories, in order.
///
/// Colours are mid-tones -- the 600 step of the usual palettes -- so each
/// reads as itself on a white page and on a dark one, and no two are
/// neighbours on the colour wheel.
pub const STARTERS: &[Starter] = &[
    Starter { builtin: "work", name: "Work", color: "#2563eb" },
    Starter { builtin: "communication", name: "Communication", color: "#0d9488" },
    Starter { builtin: "social", name: "Social", color: "#db2777" },
    Starter { builtin: "entertainment", name: "Entertainment", color: "#9333ea" },
    Starter { builtin: "learning", name: "Learning", color: "#d97706" },
    Starter { builtin: "utilities", name: "Utilities", color: "#64748b" },
];

/// The starter categories as records, `sort_order` set, ready to store.
pub fn starter_categories() -> Vec<ScreenCategory> {
    STARTERS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut category = ScreenCategory::new(s.name);
            category.builtin = s.builtin.to_string();
            category.color = s.color.to_string();
            category.sort_order = i as i64;
            category
        })
        .collect()
}

const WORK: &str = "work";
const COMMUNICATION: &str = "communication";
const SOCIAL: &str = "social";
const ENTERTAINMENT: &str = "entertainment";
const LEARNING: &str = "learning";
const UTILITIES: &str = "utilities";

/// Well-known apps, by lower-case id (without `.exe`), and the starter
/// category each belongs to.
const APPS: &[(&str, &str)] = &[
    // ---- editors and IDEs -----------------------------------------------
    ("code", WORK),
    ("code-oss", WORK),
    ("com.visualstudio.code", WORK),
    ("com.visualstudio.code.oss", WORK),
    ("com.microsoft.vscode", WORK),
    ("com.vscodium.codium", WORK),
    ("codium", WORK),
    ("vscodium", WORK),
    ("cursor", WORK),
    ("com.todesktop.230313mzl4w4u92", WORK),
    ("zed", WORK),
    ("dev.zed.zed", WORK),
    ("sublime_text", WORK),
    ("sublime-text", WORK),
    ("com.sublimetext.three", WORK),
    ("com.sublimetext.4", WORK),
    ("jetbrains-idea", WORK),
    ("idea64", WORK),
    ("com.jetbrains.intellij", WORK),
    ("jetbrains-pycharm", WORK),
    ("pycharm64", WORK),
    ("com.jetbrains.pycharm", WORK),
    ("jetbrains-clion", WORK),
    ("clion64", WORK),
    ("jetbrains-goland", WORK),
    ("goland64", WORK),
    ("jetbrains-rustrover", WORK),
    ("rustrover64", WORK),
    ("jetbrains-webstorm", WORK),
    ("webstorm64", WORK),
    ("devenv", WORK),
    ("com.apple.dt.xcode", WORK),
    ("android-studio", WORK),
    ("studio64", WORK),
    ("emacs", WORK),
    ("org.gnu.emacs", WORK),
    ("gvim", WORK),
    ("org.vim.vim", WORK),
    ("neovide", WORK),
    ("org.gnome.texteditor", UTILITIES),
    ("gnome-text-editor", UTILITIES),
    ("org.gnome.gedit", UTILITIES),
    ("org.kde.kate", WORK),
    ("kate", WORK),
    ("notepad", UTILITIES),
    ("notepad++", UTILITIES),
    ("com.apple.textedit", UTILITIES),
    // ---- terminals --------------------------------------------------------
    ("org.gnome.ptyxis", WORK),
    ("ptyxis", WORK),
    ("org.gnome.terminal", WORK),
    ("gnome-terminal-server", WORK),
    ("gnome-terminal", WORK),
    ("org.gnome.console", WORK),
    ("kgx", WORK),
    ("org.kde.konsole", WORK),
    ("konsole", WORK),
    ("alacritty", WORK),
    ("org.alacritty.alacritty", WORK),
    ("kitty", WORK),
    ("net.kovidgoyal.kitty", WORK),
    ("foot", WORK),
    ("wezterm", WORK),
    ("org.wezfurlong.wezterm", WORK),
    ("com.github.wez.wezterm", WORK),
    ("com.mitchellh.ghostty", WORK),
    ("ghostty", WORK),
    ("windowsterminal", WORK),
    ("wt", WORK),
    ("cmd", WORK),
    ("powershell", WORK),
    ("pwsh", WORK),
    ("com.apple.terminal", WORK),
    ("com.googlecode.iterm2", WORK),
    ("dev.warp.warp-stable", WORK),
    ("xterm", WORK),
    ("tilix", WORK),
    ("com.gexperts.tilix", WORK),
    // ---- office and notes -------------------------------------------------
    ("libreoffice", WORK),
    ("libreoffice-writer", WORK),
    ("libreoffice-calc", WORK),
    ("libreoffice-impress", WORK),
    ("soffice", WORK),
    ("org.libreoffice.libreoffice", WORK),
    ("winword", WORK),
    ("excel", WORK),
    ("powerpnt", WORK),
    ("onenote", WORK),
    ("com.microsoft.word", WORK),
    ("com.microsoft.excel", WORK),
    ("com.microsoft.powerpoint", WORK),
    ("com.microsoft.onenote.mac", WORK),
    ("com.apple.iwork.pages", WORK),
    ("com.apple.iwork.numbers", WORK),
    ("com.apple.iwork.keynote", WORK),
    ("obsidian", WORK),
    ("md.obsidian", WORK),
    ("notion", WORK),
    ("notion.id", WORK),
    ("com.notion.id", WORK),
    ("logseq", WORK),
    ("figma", WORK),
    ("com.figma.desktop", WORK),
    ("figma-linux", WORK),
    ("org.gimp.gimp", WORK),
    ("gimp", WORK),
    ("org.inkscape.inkscape", WORK),
    ("inkscape", WORK),
    ("org.blender.blender", WORK),
    ("blender", WORK),
    ("everyday", UTILITIES),
    ("org.everyday.app", UTILITIES),
    // ---- communication ----------------------------------------------------
    ("slack", COMMUNICATION),
    ("com.slack.slack", COMMUNICATION),
    ("com.tinyspeck.slackmacgap", COMMUNICATION),
    ("discord", COMMUNICATION),
    ("com.discordapp.discord", COMMUNICATION),
    ("com.hnc.discord", COMMUNICATION),
    ("teams", COMMUNICATION),
    ("ms-teams", COMMUNICATION),
    ("com.microsoft.teams", COMMUNICATION),
    ("com.microsoft.teams2", COMMUNICATION),
    ("zoom", COMMUNICATION),
    ("zoom.us", COMMUNICATION),
    ("us.zoom.zoom", COMMUNICATION),
    ("us.zoom.xos", COMMUNICATION),
    ("signal", COMMUNICATION),
    ("signal-desktop", COMMUNICATION),
    ("org.signal.signal", COMMUNICATION),
    ("org.whispersystems.signal-desktop", COMMUNICATION),
    ("telegram-desktop", COMMUNICATION),
    ("telegram", COMMUNICATION),
    ("org.telegram.desktop", COMMUNICATION),
    ("ru.keepcoder.telegram", COMMUNICATION),
    ("whatsapp", COMMUNICATION),
    ("net.whatsapp.whatsapp", COMMUNICATION),
    ("element", COMMUNICATION),
    ("im.riot.riot", COMMUNICATION),
    ("im.riot.element", COMMUNICATION),
    ("thunderbird", COMMUNICATION),
    ("org.mozilla.thunderbird", COMMUNICATION),
    ("outlook", COMMUNICATION),
    ("olk", COMMUNICATION),
    ("com.microsoft.outlook", COMMUNICATION),
    ("com.apple.mail", COMMUNICATION),
    ("com.apple.mobilesms", COMMUNICATION),
    ("com.apple.facetime", COMMUNICATION),
    ("org.gnome.evolution", COMMUNICATION),
    ("evolution", COMMUNICATION),
    ("org.gnome.geary", COMMUNICATION),
    ("geary", COMMUNICATION),
    ("skype", COMMUNICATION),
    ("com.skype.skype", COMMUNICATION),
    ("mattermost", COMMUNICATION),
    ("com.mattermost.desktop", COMMUNICATION),
    // ---- music and video --------------------------------------------------
    ("spotify", ENTERTAINMENT),
    ("com.spotify.client", ENTERTAINMENT),
    ("com.spotify.client.desktop", ENTERTAINMENT),
    ("vlc", ENTERTAINMENT),
    ("org.videolan.vlc", ENTERTAINMENT),
    ("mpv", ENTERTAINMENT),
    ("io.mpv.mpv", ENTERTAINMENT),
    ("celluloid", ENTERTAINMENT),
    ("io.github.celluloid_player.celluloid", ENTERTAINMENT),
    ("totem", ENTERTAINMENT),
    ("org.gnome.totem", ENTERTAINMENT),
    ("rhythmbox", ENTERTAINMENT),
    ("org.gnome.rhythmbox3", ENTERTAINMENT),
    ("com.apple.music", ENTERTAINMENT),
    ("com.apple.tv", ENTERTAINMENT),
    ("com.apple.podcasts", ENTERTAINMENT),
    ("itunes", ENTERTAINMENT),
    ("music.ui", ENTERTAINMENT),
    ("tidal", ENTERTAINMENT),
    ("plex", ENTERTAINMENT),
    ("tv.plex.plexdesktop", ENTERTAINMENT),
    ("kodi", ENTERTAINMENT),
    ("tv.kodi.kodi", ENTERTAINMENT),
    ("steam", ENTERTAINMENT),
    ("steamwebhelper", ENTERTAINMENT),
    ("com.valvesoftware.steam", ENTERTAINMENT),
    ("lutris", ENTERTAINMENT),
    ("net.lutris.lutris", ENTERTAINMENT),
    ("heroic", ENTERTAINMENT),
    ("com.heroicgameslauncher.hgl", ENTERTAINMENT),
    ("epicgameslauncher", ENTERTAINMENT),
    ("battle.net", ENTERTAINMENT),
    ("minecraft", ENTERTAINMENT),
    ("minecraftlauncher", ENTERTAINMENT),
    // ---- reading and learning ---------------------------------------------
    ("anki", LEARNING),
    ("net.ankiweb.anki", LEARNING),
    ("calibre", LEARNING),
    ("calibre-gui", LEARNING),
    ("com.calibre-ebook.calibre", LEARNING),
    ("com.calibre-ebook.ebook-viewer", LEARNING),
    ("foliate", LEARNING),
    ("com.github.johnfactotum.foliate", LEARNING),
    ("com.apple.ibooksx", LEARNING),
    ("kindle", LEARNING),
    ("com.amazon.kindle", LEARNING),
    ("zotero", LEARNING),
    ("org.zotero.zotero", LEARNING),
    ("duolingo", LEARNING),
    // ---- the desktop itself -----------------------------------------------
    ("org.gnome.nautilus", UTILITIES),
    ("nautilus", UTILITIES),
    ("org.kde.dolphin", UTILITIES),
    ("dolphin", UTILITIES),
    ("thunar", UTILITIES),
    ("explorer", UTILITIES),
    ("com.apple.finder", UTILITIES),
    ("org.gnome.settings", UTILITIES),
    ("gnome-control-center", UTILITIES),
    ("systemsettings", UTILITIES),
    ("com.apple.systempreferences", UTILITIES),
    ("org.gnome.calculator", UTILITIES),
    ("gnome-calculator", UTILITIES),
    ("calculatorapp", UTILITIES),
    ("com.apple.calculator", UTILITIES),
    ("org.gnome.calendar", UTILITIES),
    ("com.apple.ical", UTILITIES),
    ("org.gnome.software", UTILITIES),
    ("org.gnome.evince", UTILITIES),
    ("evince", UTILITIES),
    ("org.gnome.papers", UTILITIES),
    ("org.kde.okular", UTILITIES),
    ("okular", UTILITIES),
    ("acrord32", UTILITIES),
    ("acrobat", UTILITIES),
    ("com.adobe.acrobat.pro", UTILITIES),
    ("com.apple.preview", UTILITIES),
    ("org.gnome.loupe", UTILITIES),
    ("org.gnome.eog", UTILITIES),
    ("eog", UTILITIES),
    ("1password", UTILITIES),
    ("com.1password.1password", UTILITIES),
    ("bitwarden", UTILITIES),
    ("com.bitwarden.desktop", UTILITIES),
    ("keepassxc", UTILITIES),
    ("org.keepassxc.keepassxc", UTILITIES),
    ("taskmgr", UTILITIES),
    ("org.gnome.systemmonitor", UTILITIES),
    ("gnome-system-monitor", UTILITIES),
    ("com.apple.activitymonitor", UTILITIES),
];

/// Well-known sites, by registrable domain (or a host, for the few that
/// are worth telling apart from their domain), and the starter category
/// each belongs to. A host not listed falls back to its parent domains:
/// `mail.google.com` finds `google.com`.
const SITES: &[(&str, &str)] = &[
    // ---- work -------------------------------------------------------------
    ("github.com", WORK),
    ("gitlab.com", WORK),
    ("bitbucket.org", WORK),
    ("codeberg.org", WORK),
    ("sr.ht", WORK),
    ("atlassian.net", WORK),
    ("atlassian.com", WORK),
    ("linear.app", WORK),
    ("notion.so", WORK),
    ("notion.site", WORK),
    ("figma.com", WORK),
    ("miro.com", WORK),
    ("trello.com", WORK),
    ("asana.com", WORK),
    ("monday.com", WORK),
    ("clickup.com", WORK),
    ("airtable.com", WORK),
    ("vercel.com", WORK),
    ("netlify.com", WORK),
    ("cloudflare.com", WORK),
    ("aws.amazon.com", WORK),
    ("console.cloud.google.com", WORK),
    ("portal.azure.com", WORK),
    ("azure.com", WORK),
    ("office.com", WORK),
    ("office365.com", WORK),
    ("sharepoint.com", WORK),
    ("crates.io", WORK),
    ("docs.rs", WORK),
    ("rust-lang.org", WORK),
    ("npmjs.com", WORK),
    ("pypi.org", WORK),
    ("localhost", WORK),
    ("127.0.0.1", WORK),
    ("chatgpt.com", WORK),
    ("openai.com", WORK),
    ("claude.ai", WORK),
    ("anthropic.com", WORK),
    ("gemini.google.com", WORK),
    ("perplexity.ai", WORK),
    // ---- communication ----------------------------------------------------
    ("mail.google.com", COMMUNICATION),
    ("outlook.live.com", COMMUNICATION),
    ("outlook.office.com", COMMUNICATION),
    ("outlook.com", COMMUNICATION),
    ("live.com", COMMUNICATION),
    ("proton.me", COMMUNICATION),
    ("protonmail.com", COMMUNICATION),
    ("fastmail.com", COMMUNICATION),
    ("mail.yahoo.com", COMMUNICATION),
    ("slack.com", COMMUNICATION),
    ("discord.com", COMMUNICATION),
    ("teams.microsoft.com", COMMUNICATION),
    ("teams.live.com", COMMUNICATION),
    ("zoom.us", COMMUNICATION),
    ("meet.google.com", COMMUNICATION),
    ("chat.google.com", COMMUNICATION),
    ("whatsapp.com", COMMUNICATION),
    ("web.whatsapp.com", COMMUNICATION),
    ("telegram.org", COMMUNICATION),
    ("web.telegram.org", COMMUNICATION),
    ("messenger.com", COMMUNICATION),
    ("app.element.io", COMMUNICATION),
    ("element.io", COMMUNICATION),
    ("signal.org", COMMUNICATION),
    ("icloud.com", COMMUNICATION),
    // ---- social -----------------------------------------------------------
    ("facebook.com", SOCIAL),
    ("instagram.com", SOCIAL),
    ("x.com", SOCIAL),
    ("twitter.com", SOCIAL),
    ("reddit.com", SOCIAL),
    ("linkedin.com", SOCIAL),
    ("tiktok.com", SOCIAL),
    ("threads.net", SOCIAL),
    ("threads.com", SOCIAL),
    ("bsky.app", SOCIAL),
    ("mastodon.social", SOCIAL),
    ("pinterest.com", SOCIAL),
    ("tumblr.com", SOCIAL),
    ("snapchat.com", SOCIAL),
    ("news.ycombinator.com", SOCIAL),
    ("lobste.rs", SOCIAL),
    ("quora.com", SOCIAL),
    ("nextdoor.com", SOCIAL),
    ("vk.com", SOCIAL),
    ("weibo.com", SOCIAL),
    // ---- entertainment ----------------------------------------------------
    ("youtube.com", ENTERTAINMENT),
    ("youtu.be", ENTERTAINMENT),
    ("netflix.com", ENTERTAINMENT),
    ("primevideo.com", ENTERTAINMENT),
    ("disneyplus.com", ENTERTAINMENT),
    ("hulu.com", ENTERTAINMENT),
    ("max.com", ENTERTAINMENT),
    ("hbomax.com", ENTERTAINMENT),
    ("tv.apple.com", ENTERTAINMENT),
    ("music.apple.com", ENTERTAINMENT),
    ("peacocktv.com", ENTERTAINMENT),
    ("paramountplus.com", ENTERTAINMENT),
    ("crunchyroll.com", ENTERTAINMENT),
    ("twitch.tv", ENTERTAINMENT),
    ("kick.com", ENTERTAINMENT),
    ("spotify.com", ENTERTAINMENT),
    ("open.spotify.com", ENTERTAINMENT),
    ("soundcloud.com", ENTERTAINMENT),
    ("bandcamp.com", ENTERTAINMENT),
    ("music.youtube.com", ENTERTAINMENT),
    ("pandora.com", ENTERTAINMENT),
    ("tidal.com", ENTERTAINMENT),
    ("imdb.com", ENTERTAINMENT),
    ("letterboxd.com", ENTERTAINMENT),
    ("9gag.com", ENTERTAINMENT),
    ("imgur.com", ENTERTAINMENT),
    ("store.steampowered.com", ENTERTAINMENT),
    ("steampowered.com", ENTERTAINMENT),
    ("steamcommunity.com", ENTERTAINMENT),
    ("epicgames.com", ENTERTAINMENT),
    ("chess.com", ENTERTAINMENT),
    ("lichess.org", ENTERTAINMENT),
    ("espn.com", ENTERTAINMENT),
    // ---- learning, reading and the news -----------------------------------
    ("wikipedia.org", LEARNING),
    ("wikimedia.org", LEARNING),
    ("wiktionary.org", LEARNING),
    ("stackoverflow.com", LEARNING),
    ("stackexchange.com", LEARNING),
    ("superuser.com", LEARNING),
    ("serverfault.com", LEARNING),
    ("askubuntu.com", LEARNING),
    ("developer.mozilla.org", LEARNING),
    ("coursera.org", LEARNING),
    ("edx.org", LEARNING),
    ("khanacademy.org", LEARNING),
    ("udemy.com", LEARNING),
    ("udacity.com", LEARNING),
    ("skillshare.com", LEARNING),
    ("brilliant.org", LEARNING),
    ("duolingo.com", LEARNING),
    ("arxiv.org", LEARNING),
    ("scholar.google.com", LEARNING),
    ("medium.com", LEARNING),
    ("substack.com", LEARNING),
    ("goodreads.com", LEARNING),
    ("nytimes.com", LEARNING),
    ("washingtonpost.com", LEARNING),
    ("wsj.com", LEARNING),
    ("theguardian.com", LEARNING),
    ("bbc.co.uk", LEARNING),
    ("bbc.com", LEARNING),
    ("cnn.com", LEARNING),
    ("reuters.com", LEARNING),
    ("apnews.com", LEARNING),
    ("npr.org", LEARNING),
    ("economist.com", LEARNING),
    ("ft.com", LEARNING),
    ("bloomberg.com", LEARNING),
    ("theatlantic.com", LEARNING),
    ("newyorker.com", LEARNING),
    ("arstechnica.com", LEARNING),
    ("theverge.com", LEARNING),
    ("wired.com", LEARNING),
    ("lwn.net", LEARNING),
    ("news.google.com", LEARNING),
    // ---- utilities --------------------------------------------------------
    ("google.com", UTILITIES),
    ("bing.com", UTILITIES),
    ("duckduckgo.com", UTILITIES),
    ("kagi.com", UTILITIES),
    ("startpage.com", UTILITIES),
    ("search.brave.com", UTILITIES),
    ("yahoo.com", UTILITIES),
    ("maps.apple.com", UTILITIES),
    ("openstreetmap.org", UTILITIES),
    ("dropbox.com", UTILITIES),
    ("box.com", UTILITIES),
    ("onedrive.live.com", UTILITIES),
    ("translate.google.com", UTILITIES),
    ("deepl.com", UTILITIES),
    ("amazon.com", UTILITIES),
    ("amazon.co.uk", UTILITIES),
    ("ebay.com", UTILITIES),
    ("etsy.com", UTILITIES),
    ("paypal.com", UTILITIES),
    ("weather.com", UTILITIES),
    ("accuweather.com", UTILITIES),
];

/// The usual ids of web browsers on every platform, lower-case and without
/// `.exe` or `.desktop`: desktop-file and Flatpak ids, X11 classes,
/// executables, and macOS bundle ids.
const BROWSERS: &[&str] = &[
    // Firefox and its family.
    "firefox",
    "firefox-esr",
    "firefox-bin",
    "firefox-developer-edition",
    "firefox-nightly",
    "firefoxdeveloperedition",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "org.mozilla.nightly",
    "navigator",
    "librewolf",
    "io.gitlab.librewolf-community",
    "waterfox",
    "net.waterfox.waterfox",
    "floorp",
    "one.ablaze.floorp",
    "zen",
    "zen-browser",
    "zen-alpha",
    "app.zen_browser.zen",
    "io.github.zen_browser.zen",
    "mullvad-browser",
    "mullvadbrowser",
    "net.mullvad.mullvadbrowser",
    "tor-browser",
    "torbrowser",
    "tor browser",
    "org.torproject.torbrowser",
    "org.torproject.torbrowser-launcher",
    "com.github.micahflee.torbrowser-launcher",
    "palemoon",
    "seamonkey",
    "basilisk",
    // Chrome and Chromium.
    "google-chrome",
    "google-chrome-stable",
    "google-chrome-beta",
    "google-chrome-unstable",
    "google-chrome-dev",
    "chrome",
    "com.google.chrome",
    "com.google.chrome.beta",
    "com.google.chrome.dev",
    "com.google.chrome.canary",
    "chromium",
    "chromium-browser",
    "org.chromium.chromium",
    "ungoogled-chromium",
    "io.github.ungoogled_software.ungoogled_chromium",
    "com.github.eloston.ungoogledchromium",
    "thorium-browser",
    "thorium",
    // Edge.
    "microsoft-edge",
    "microsoft-edge-stable",
    "microsoft-edge-beta",
    "microsoft-edge-dev",
    "msedge",
    "com.microsoft.edge",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.beta",
    // Brave, Vivaldi, Opera.
    "brave",
    "brave-browser",
    "brave-browser-stable",
    "brave-browser-beta",
    "com.brave.browser",
    "com.brave.browser.beta",
    "vivaldi",
    "vivaldi-stable",
    "vivaldi-snapshot",
    "com.vivaldi.vivaldi",
    "opera",
    "opera-beta",
    "opera_gx",
    "com.opera.opera",
    "com.operasoftware.opera",
    "com.operasoftware.operagx",
    "yandex-browser",
    "browser",
    "ru.yandex.desktop.yandex-browser",
    // Safari, Arc, Orion and the other Mac ones.
    "safari",
    "com.apple.safari",
    "com.apple.safaritechnologypreview",
    "arc",
    "company.thebrowser.browser",
    "company.thebrowser.dia",
    "orion",
    "com.kagi.kagimacos",
    "duckduckgo",
    "com.duckduckgo.macos.browser",
    "sidekick",
    "wavebox",
    "min",
    // Linux's own.
    "epiphany",
    "org.gnome.epiphany",
    "falkon",
    "org.kde.falkon",
    "konqueror",
    "org.kde.konqueror",
    "qutebrowser",
    "org.qutebrowser.qutebrowser",
    "midori",
    "org.midori_browser.midori",
    "nyxt",
    "ladybird",
    "org.ladybird.ladybird",
    "iexplore",
];

/// Friendlier names for well-known app ids, lower-case and without `.exe`
/// or `.desktop`.
const NAMES: &[(&str, &str)] = &[
    // Browsers.
    ("firefox", "Firefox"),
    ("firefox-esr", "Firefox"),
    ("org.mozilla.firefox", "Firefox"),
    ("navigator", "Firefox"),
    ("librewolf", "LibreWolf"),
    ("io.gitlab.librewolf-community", "LibreWolf"),
    ("floorp", "Floorp"),
    ("one.ablaze.floorp", "Floorp"),
    ("zen", "Zen"),
    ("zen-browser", "Zen"),
    ("app.zen_browser.zen", "Zen"),
    ("google-chrome", "Chrome"),
    ("google-chrome-stable", "Chrome"),
    ("chrome", "Chrome"),
    ("com.google.chrome", "Chrome"),
    ("chromium", "Chromium"),
    ("chromium-browser", "Chromium"),
    ("org.chromium.chromium", "Chromium"),
    ("microsoft-edge", "Edge"),
    ("microsoft-edge-stable", "Edge"),
    ("msedge", "Edge"),
    ("com.microsoft.edge", "Edge"),
    ("com.microsoft.edgemac", "Edge"),
    ("brave", "Brave"),
    ("brave-browser", "Brave"),
    ("com.brave.browser", "Brave"),
    ("vivaldi", "Vivaldi"),
    ("vivaldi-stable", "Vivaldi"),
    ("com.vivaldi.vivaldi", "Vivaldi"),
    ("opera", "Opera"),
    ("com.operasoftware.opera", "Opera"),
    ("safari", "Safari"),
    ("com.apple.safari", "Safari"),
    ("arc", "Arc"),
    ("company.thebrowser.browser", "Arc"),
    ("epiphany", "Web"),
    ("org.gnome.epiphany", "Web"),
    ("qutebrowser", "qutebrowser"),
    // Editors and terminals.
    ("code", "Visual Studio Code"),
    ("code-oss", "Code - OSS"),
    ("com.visualstudio.code", "Visual Studio Code"),
    ("com.microsoft.vscode", "Visual Studio Code"),
    ("codium", "VSCodium"),
    ("com.vscodium.codium", "VSCodium"),
    ("cursor", "Cursor"),
    ("zed", "Zed"),
    ("dev.zed.zed", "Zed"),
    ("sublime_text", "Sublime Text"),
    ("jetbrains-idea", "IntelliJ IDEA"),
    ("idea64", "IntelliJ IDEA"),
    ("jetbrains-pycharm", "PyCharm"),
    ("pycharm64", "PyCharm"),
    ("jetbrains-rustrover", "RustRover"),
    ("rustrover64", "RustRover"),
    ("devenv", "Visual Studio"),
    ("com.apple.dt.xcode", "Xcode"),
    ("emacs", "Emacs"),
    ("org.gnu.emacs", "Emacs"),
    ("gvim", "Vim"),
    ("neovide", "Neovide"),
    ("org.gnome.texteditor", "Text Editor"),
    ("gnome-text-editor", "Text Editor"),
    ("org.kde.kate", "Kate"),
    ("notepad", "Notepad"),
    ("notepad++", "Notepad++"),
    ("org.gnome.ptyxis", "Terminal"),
    ("ptyxis", "Terminal"),
    ("org.gnome.terminal", "Terminal"),
    ("gnome-terminal-server", "Terminal"),
    ("org.gnome.console", "Console"),
    ("kgx", "Console"),
    ("org.kde.konsole", "Konsole"),
    ("konsole", "Konsole"),
    ("alacritty", "Alacritty"),
    ("kitty", "kitty"),
    ("foot", "foot"),
    ("wezterm", "WezTerm"),
    ("org.wezfurlong.wezterm", "WezTerm"),
    ("com.mitchellh.ghostty", "Ghostty"),
    ("ghostty", "Ghostty"),
    ("windowsterminal", "Windows Terminal"),
    ("cmd", "Command Prompt"),
    ("powershell", "PowerShell"),
    ("pwsh", "PowerShell"),
    ("com.apple.terminal", "Terminal"),
    ("com.googlecode.iterm2", "iTerm2"),
    // Office and notes.
    ("libreoffice-writer", "LibreOffice Writer"),
    ("libreoffice-calc", "LibreOffice Calc"),
    ("libreoffice-impress", "LibreOffice Impress"),
    ("soffice", "LibreOffice"),
    ("winword", "Word"),
    ("excel", "Excel"),
    ("powerpnt", "PowerPoint"),
    ("com.microsoft.word", "Word"),
    ("com.microsoft.excel", "Excel"),
    ("com.microsoft.powerpoint", "PowerPoint"),
    ("obsidian", "Obsidian"),
    ("md.obsidian", "Obsidian"),
    ("notion", "Notion"),
    ("com.notion.id", "Notion"),
    ("figma", "Figma"),
    ("org.gimp.gimp", "GIMP"),
    ("org.inkscape.inkscape", "Inkscape"),
    ("org.blender.blender", "Blender"),
    ("everyday", "Every Day"),
    // Communication.
    ("slack", "Slack"),
    ("com.slack.slack", "Slack"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("discord", "Discord"),
    ("com.discordapp.discord", "Discord"),
    ("teams", "Teams"),
    ("ms-teams", "Teams"),
    ("com.microsoft.teams2", "Teams"),
    ("zoom", "Zoom"),
    ("us.zoom.xos", "Zoom"),
    ("signal", "Signal"),
    ("signal-desktop", "Signal"),
    ("org.signal.signal", "Signal"),
    ("telegram-desktop", "Telegram"),
    ("org.telegram.desktop", "Telegram"),
    ("whatsapp", "WhatsApp"),
    ("element", "Element"),
    ("im.riot.riot", "Element"),
    ("thunderbird", "Thunderbird"),
    ("org.mozilla.thunderbird", "Thunderbird"),
    ("outlook", "Outlook"),
    ("olk", "Outlook"),
    ("com.microsoft.outlook", "Outlook"),
    ("com.apple.mail", "Mail"),
    ("com.apple.mobilesms", "Messages"),
    ("org.gnome.evolution", "Evolution"),
    ("org.gnome.geary", "Geary"),
    // Music and video.
    ("spotify", "Spotify"),
    ("com.spotify.client", "Spotify"),
    ("vlc", "VLC"),
    ("org.videolan.vlc", "VLC"),
    ("mpv", "mpv"),
    ("io.mpv.mpv", "mpv"),
    ("io.github.celluloid_player.celluloid", "Celluloid"),
    ("org.gnome.totem", "Videos"),
    ("org.gnome.rhythmbox3", "Rhythmbox"),
    ("com.apple.music", "Music"),
    ("com.apple.tv", "TV"),
    ("com.apple.podcasts", "Podcasts"),
    ("steam", "Steam"),
    ("steamwebhelper", "Steam"),
    ("com.valvesoftware.steam", "Steam"),
    // Reading and learning.
    ("anki", "Anki"),
    ("net.ankiweb.anki", "Anki"),
    ("calibre-gui", "calibre"),
    ("com.github.johnfactotum.foliate", "Foliate"),
    ("com.apple.ibooksx", "Books"),
    ("org.zotero.zotero", "Zotero"),
    // The desktop itself.
    ("org.gnome.nautilus", "Files"),
    ("nautilus", "Files"),
    ("org.kde.dolphin", "Dolphin"),
    ("explorer", "File Explorer"),
    ("com.apple.finder", "Finder"),
    ("org.gnome.settings", "Settings"),
    ("gnome-control-center", "Settings"),
    ("com.apple.systempreferences", "System Settings"),
    ("org.gnome.calculator", "Calculator"),
    ("org.gnome.calendar", "Calendar"),
    ("org.gnome.evince", "Document Viewer"),
    ("org.gnome.papers", "Papers"),
    ("org.kde.okular", "Okular"),
    ("com.apple.preview", "Preview"),
    ("org.gnome.loupe", "Image Viewer"),
    ("1password", "1Password"),
    ("bitwarden", "Bitwarden"),
    ("org.keepassxc.keepassxc", "KeePassXC"),
    ("taskmgr", "Task Manager"),
];

/// An app id as the tables key it: lower-case, trimmed, and the ids to try
/// in turn -- as given, then without a trailing `.exe` or `.desktop`.
fn app_ids(app: &str) -> impl Iterator<Item = String> {
    let id = app.trim().to_lowercase();
    let bare = id.strip_suffix(".exe").or_else(|| id.strip_suffix(".desktop")).map(str::to_string);
    std::iter::once(id).chain(bare)
}

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The starter category (its `builtin` key) suggested for an item key
/// (`app:org.gnome.ptyxis`, `site:youtube.com`), if any.
///
/// A site's key may be a host (a domain whose hosts are kept apart): it is
/// looked up as it is and then by each parent domain in turn, so
/// `site:docs.google.com` finds `google.com`'s suggestion, while
/// `site:mail.google.com` finds its own.
pub fn suggest(key: &str) -> Option<&'static str> {
    let (prefix, rest) = key.split_once(':')?;
    match prefix {
        "app" => app_ids(rest).find_map(|id| lookup(APPS, &id)),
        "site" => {
            let mut host = rest.trim().to_lowercase();
            loop {
                if let Some(found) = lookup(SITES, &host) {
                    return Some(found);
                }
                // `a.b.c` → `b.c`. A bare suffix (`com`) is never looked
                // up: a suggestion for a whole top-level domain would be a
                // guess about millions of sites.
                let (_, parent) = host.split_once('.')?;
                if !parent.contains('.') {
                    return None;
                }
                host = parent.to_string();
            }
        }
        _ => None,
    }
}

/// Is this app id a web browser? Case-insensitive; knows the usual ids on
/// every platform (`firefox`, `org.mozilla.firefox`, `firefox.exe`,
/// `google-chrome`, `chrome.exe`, `com.google.Chrome`, `com.apple.Safari`,
/// `msedge.exe`, `brave-browser`, `vivaldi`, `opera`, `arc`, ...).
pub fn is_browser(app: &str) -> bool {
    app_ids(app).any(|id| BROWSERS.contains(&id.as_str()))
}

/// A friendlier name for a well-known app id (`org.gnome.Ptyxis` →
/// `Terminal`, `code.exe` → `Visual Studio Code`), if one is known.
pub fn friendly_name(app: &str) -> Option<&'static str> {
    app_ids(app).find_map(|id| lookup(NAMES, &id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_starters_are_six_distinct_categories_in_order() {
        let starters = starter_categories();
        assert_eq!(starters.len(), 6);
        let builtins: Vec<&str> = starters.iter().map(|c| c.builtin.as_str()).collect();
        assert_eq!(
            builtins,
            ["work", "communication", "social", "entertainment", "learning", "utilities"]
        );
        for (i, c) in starters.iter().enumerate() {
            assert_eq!(c.sort_order, i as i64);
            assert!(!c.name.is_empty());
            assert_eq!(c.color.len(), 7, "{}", c.color);
            assert!(c.color.starts_with('#'));
            assert!(c.color[1..].chars().all(|ch| ch.is_ascii_hexdigit()), "{}", c.color);
        }
        let ids: HashSet<_> = starters.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 6);
    }

    #[test]
    fn every_suggestion_names_a_starter_and_every_key_is_written_as_keys_are() {
        let builtins: HashSet<&str> = STARTERS.iter().map(|s| s.builtin).collect();
        for (key, category) in APPS.iter().chain(SITES) {
            assert!(builtins.contains(category), "{key} → {category}");
            assert_eq!(*key, key.trim().to_lowercase(), "{key} is not a lower-case key");
            // `.exe` is stripped before looking up, so a key with it could
            // never be found. (`.desktop` may stay: `org.telegram.desktop`
            // is an id in its own right, and the id is tried as it came
            // before anything is stripped.)
            assert!(!key.ends_with(".exe"), "{key}");
        }
        assert!(APPS.len() + SITES.len() >= 150);
    }

    #[test]
    fn no_table_lists_a_key_twice() {
        for table in [APPS, SITES, NAMES] {
            let mut seen = HashSet::new();
            for (key, _) in table {
                assert!(seen.insert(*key), "{key} is listed twice");
            }
        }
        let mut seen = HashSet::new();
        for id in BROWSERS {
            assert!(seen.insert(*id), "{id} is listed twice");
            assert_eq!(*id, id.to_lowercase());
        }
    }

    #[test]
    fn apps_are_suggested_whatever_platform_named_them() {
        assert_eq!(suggest("app:org.gnome.ptyxis"), Some("work"));
        assert_eq!(suggest("app:code.exe"), Some("work"));
        assert_eq!(suggest("app:com.microsoft.vscode"), Some("work"));
        assert_eq!(suggest("app:slack"), Some("communication"));
        assert_eq!(suggest("app:spotify.exe"), Some("entertainment"));
        assert_eq!(suggest("app:org.gnome.nautilus.desktop"), Some("utilities"));
        assert_eq!(suggest("app:something-nobody-knows"), None);
    }

    #[test]
    fn a_site_falls_back_to_its_parent_domains() {
        assert_eq!(suggest("site:youtube.com"), Some("entertainment"));
        assert_eq!(suggest("site:github.com"), Some("work"));
        assert_eq!(suggest("site:google.com"), Some("utilities"));
        assert_eq!(suggest("site:docs.google.com"), Some("utilities"));
        assert_eq!(suggest("site:mail.google.com"), Some("communication"));
        assert_eq!(suggest("site:en.wikipedia.org"), Some("learning"));
        assert_eq!(suggest("site:old.reddit.com"), Some("social"));
        assert_eq!(suggest("site:example.com"), None);
        assert_eq!(suggest("site:com"), None);
        assert_eq!(suggest("nonsense"), None);
        assert_eq!(suggest("thing:github.com"), None);
    }

    #[test]
    fn browsers_are_known_on_every_platform() {
        for id in [
            "firefox",
            "Firefox",
            "org.mozilla.firefox",
            "firefox.exe",
            "google-chrome",
            "chrome.exe",
            "com.google.Chrome",
            "com.apple.Safari",
            "msedge.exe",
            "brave-browser",
            "vivaldi",
            "opera",
            "arc",
            "org.gnome.Epiphany",
        ] {
            assert!(is_browser(id), "{id}");
        }
        for id in ["code", "org.gnome.Ptyxis", "slack", ""] {
            assert!(!is_browser(id), "{id}");
        }
    }

    #[test]
    fn common_ids_have_friendly_names() {
        assert_eq!(friendly_name("org.gnome.Ptyxis"), Some("Terminal"));
        assert_eq!(friendly_name("code.exe"), Some("Visual Studio Code"));
        assert_eq!(friendly_name("Code.exe"), Some("Visual Studio Code"));
        assert_eq!(friendly_name("com.apple.Safari"), Some("Safari"));
        assert_eq!(friendly_name("unknown.app"), None);
    }
}
