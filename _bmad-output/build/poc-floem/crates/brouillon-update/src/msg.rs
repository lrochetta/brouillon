//! User-visible messages (status bar), French or English.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
}

pub enum Msg<'a> {
    Installing(&'a str),
    Updated(&'a str),
    OnExit(&'a str),
    Available { version: &'a str, url: &'a str },
    Required { secs: u32 },
    Failed(&'a str),
    InstallFailed(&'a str),
    /// A newer build was found and its installer is being downloaded.
    Downloading { version: &'a str, build: u64, pct: u32 },
    /// Verified installer waiting. `dirty`: there is unsaved work (journaled first).
    Ready { dirty: bool },
    /// The installer ran but did not apply: manual route with the signed file name, hash and link.
    CouldNotInstall { file: &'a str, sha256: &'a str, url: &'a str },
    /// Portable copy: nothing is installed, the user gets the file name, hash and link.
    Portable { version: &'a str, build: u64, file: &'a str, sha256: &'a str, url: &'a str },
}

pub fn render(m: &Msg, l: Lang) -> String {
    use Lang::*;
    match (m, l) {
        (Msg::Installing(v), Fr) => format!("Mise à jour vers {v} en cours…"),
        (Msg::Installing(v), En) => format!("Updating to {v}…"),
        (Msg::Updated(v), Fr) => format!("Mis à jour vers {v}"),
        (Msg::Updated(v), En) => format!("Updated to {v}"),
        // Short on purpose: the status bar cuts long messages.
        (Msg::OnExit(v), Fr) => format!("MAJ {v} à la fermeture"),
        (Msg::OnExit(v), En) => format!("Update {v} ready: installs on close"),
        (Msg::Available { version, url }, Fr) => format!("Une nouvelle version existe : {version} ({url})"),
        (Msg::Available { version, url }, En) => format!("A new version exists: {version} ({url})"),
        (Msg::Required { secs }, Fr) => format!("MAJ REQUISE dans {secs} s, travail sauvegardé"),
        (Msg::Required { secs }, En) => format!("UPDATE REQUIRED in {secs} s, work saved"),
        (Msg::Failed(e), Fr) => format!("Mise à jour impossible ({e}). Nouvel essai plus tard ; l'éditeur continue normalement."),
        (Msg::Failed(e), En) => format!("Update not possible ({e}). Will retry later; the editor keeps working."),
        (Msg::Downloading { version, build, pct }, Fr) => format!("La mise à jour {version} (build {build}) est disponible : téléchargement… {pct} %"),
        (Msg::Downloading { version, build, pct }, En) => format!("Update {version} (build {build}) is available: downloading... {pct}%"),
        (Msg::Ready { dirty: false }, Fr) => "Prête : Brouillon va se fermer, se mettre à jour et redémarrer.".to_string(),
        (Msg::Ready { dirty: false }, En) => "Ready: Brouillon will close, update and restart.".to_string(),
        (Msg::Ready { dirty: true }, Fr) => "Prête : Brouillon va se fermer, se mettre à jour et redémarrer. Vos modifications non enregistrées sont sauvegardées puis restaurées.".to_string(),
        (Msg::Ready { dirty: true }, En) => "Ready: Brouillon will close, update and restart. Your unsaved work is saved first and restored afterwards.".to_string(),
        (Msg::CouldNotInstall { file, sha256, url }, Fr) => format!("La mise à jour n'a pas pu être installée automatiquement. Téléchargez le zip portable ou lancez l'installateur en administrateur : {file}, SHA-256 {sha256}, {url}"),
        (Msg::CouldNotInstall { file, sha256, url }, En) => format!("The update could not be installed automatically. Download the portable zip or run the installer as administrator: {file}, SHA-256 {sha256}, {url}"),
        (Msg::Portable { version, build, file, sha256, url }, Fr) => format!("Version portable : la mise à jour {version} (build {build}) est disponible, rien n'est installé. Fichier {file}, SHA-256 {sha256}, {url}"),
        (Msg::Portable { version, build, file, sha256, url }, En) => format!("Portable copy: update {version} (build {build}) is available, nothing is installed. File {file}, SHA-256 {sha256}, {url}"),
        (Msg::InstallFailed(v), Fr) => format!("La mise à jour vers {v} n'a pas abouti. Nouvel essai plus tard."),
        (Msg::InstallFailed(v), En) => format!("The update to {v} did not complete. Will retry later."),
    }
}

/// French when the Windows user locale (or `LC_ALL`/`LANG` elsewhere) starts with `fr`.
pub fn detect_lang() -> Lang {
    #[cfg(windows)]
    {
        extern "system" {
            fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
        }
        let mut buf = [0u16; 85];
        // SAFETY: buffer of 85 u16 (LOCALE_NAME_MAX_LENGTH) is valid for the declared length.
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if n > 1 {
            let s = String::from_utf16_lossy(&buf[..(n as usize - 1)]);
            return if s.to_ascii_lowercase().starts_with("fr") { Lang::Fr } else { Lang::En };
        }
    }
    let v = std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")).unwrap_or_default();
    if v.to_ascii_lowercase().starts_with("fr") {
        Lang::Fr
    } else {
        Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_route_message_names_file_hash_and_link() {
        let t = render(&Msg::CouldNotInstall { file: "B.zip", sha256: "ab12", url: "https://brouillon.rochetta.fr/download/" }, Lang::En);
        assert!(t.starts_with("The update could not be installed automatically. Download the portable zip or run the installer as administrator"));
        assert!(t.contains("B.zip") && t.contains("ab12") && t.contains("https://brouillon.rochetta.fr/download/"));
    }

    #[test]
    fn download_and_ready_texts_are_plain() {
        let t = render(&Msg::Downloading { version: "0.1.0-alpha.16", build: 90, pct: 42 }, Lang::En);
        assert_eq!(t, "Update 0.1.0-alpha.16 (build 90) is available: downloading... 42%");
        assert_eq!(render(&Msg::Ready { dirty: false }, Lang::En), "Ready: Brouillon will close, update and restart.");
        assert!(render(&Msg::Ready { dirty: true }, Lang::En).contains("unsaved work is saved first"));
    }

    #[test]
    fn both_languages_name_the_version() {
        for l in [Lang::En, Lang::Fr] {
            for m in [Msg::Installing("1.2"), Msg::Updated("1.2"), Msg::OnExit("1.2")] {
                assert!(render(&m, l).contains("1.2"));
            }
        }
        assert_eq!(render(&Msg::Installing("0.9.0"), Lang::Fr), "Mise à jour vers 0.9.0 en cours…");
        assert_eq!(render(&Msg::Updated("0.9.0"), Lang::Fr), "Mis à jour vers 0.9.0");
    }
}
