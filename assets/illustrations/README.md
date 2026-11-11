# Brouillon illustrations

Pure SVG, no script, no raster, no embedded third-party mark. Source: `_generate.mjs` (`node _generate.mjs` rewrites every file and `index.html`). Preview: serve this folder over HTTP and open `index.html` (auto / dark / light toggle).

## Integration
- Default: `<img src="/assets/illustrations/x.svg" alt="..." width height loading="lazy">`. Each file embeds its own `<style>` (same rules as `illustrations.css`) with dark/light via `prefers-color-scheme`. It cannot read the page theme toggle (`data-theme`), because an `<img>` is isolated.
- Inline (to follow the site toggle and the page tokens): paste the SVG, delete its `<style>` (CSP `style-src 'self'`) and load `illustrations.css` (1.7 KB). Colours come from `var(--accent, ...)`, `--agent`, `--text`, `--surface`... with coded fallbacks. Inline icons: decorative, use `aria-hidden="true"` and drop `role/title/desc` when text is next to them.
- `*.fr.svg` exist for files with visible text (hero, agent-proposes, local-first). Others have none.
- Anything agent/MCP related carries the dashed "Design preview" / "Aperçu de conception" pill. No "delivered" badge is used: nothing drawn here is a verified feature claim except the local files, backups and recovery icons.

## Weights (bytes)
hero 6.7 K, agent-proposes 4.9 K, local-first 4.2 K, download 3.0 K, notfound 2.6 K, icons ~2.1 K each, empty states ~2.2 K each.

## Alt text (EN / FR)
- hero: "A text editor with a human cursor and a ghost cursor (design preview)" / "Un éditeur de texte avec un curseur humain et un curseur fantôme (aperçu de conception)"
- agent-proposes: "The ghost cursor proposes, you accept or reject each block (design preview)" / "Le curseur fantôme propose, vous acceptez ou refusez chaque bloc (aperçu de conception)"
- local-first: "Your files stay on your machine; a cloud AI provider is optional and your choice" / "Vos fichiers restent sur votre machine ; un fournisseur d'IA dans le nuage est optionnel et à votre choix"
- download: "An installer and a zip archive, checked against a SHA-256 hash" / "Un installeur et une archive zip, vérifiés avec une empreinte SHA-256"
- notfound: "Page not found: an empty editor with a blinking cursor" / "Page introuvable : un éditeur vide avec un curseur qui clignote"
- icon-quick-start: "Quick start" / "Démarrage rapide"; icon-local-first: "Local-first and private" / "Local d'abord, privé"; icon-search-replace: "Search and replace" / "Recherche et remplacement"; icon-grouped-undo: "Grouped undo" / "Annulation groupée"; icon-crash-recovery: "Crash recovery" / "Récupération après plantage"; icon-syntax: "Syntax highlighting" / "Coloration syntaxique"; icon-palette: "Shortcuts and command palette" / "Raccourcis et palette de commandes"; icon-backups: "Durable backups" / "Sauvegardes durables"
- empty-no-results: "No results" / "Aucun résultat"; empty-nothing-to-recover: "Nothing to recover" / "Rien à récupérer"; empty-folder: "Empty folder" / "Dossier vide"

## Contrast (computed, WCAG)
Dark / light, on `--surface`: accent 6.8 / 5.8, ok 9.4 / 5.6, danger 6.5 / 5.8, faint (pending buttons) 4.6 / 5.2, agent label on agent-tint 6.6 / 5.1. Decorative frames use `--border-ctrl` (2.8, not meaning-bearing). Meaning is never colour-only: check vs cross shapes, solid vs hollow/dashed.
