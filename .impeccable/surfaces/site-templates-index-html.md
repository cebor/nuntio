---
version: 1
slug: "site-templates-index-html"
primary_target: "site/templates/index.html"
related_targets: ["site/templates/base.html","site/static/style.css"]
---

# Surface: nuntio website (home, docs, privacy)

Mode: Persuade (home), Read (docs, privacy). Audience: Linux and AI power users; Windows developers in WSL. Action: download the current release for the visitor's OS. Proof: real screenshots, exact protocol facts, the in-page image-paste simulation. Constraints: Zola 0.23 static, no third-party requests, works without JS.

## Direction contract

seed: d7a7d785 (assigned direction: Pocket Reference)

THESIS: The site is nuntio's programming pocket reference: a folded card in the lineage of DEC's VT references, the source of the CSI/OSC/DEC-mode vocabulary nuntio speaks. It refuses the dark terminal landing page with a neon accent and a feature grid.

OWN-WORLD: Warm card stock printed in black ink and one signal orange; a night card in dark mode. Condensed Archivo caps for heads, JetBrains Mono for sequences and keys. Ruled reference tables, keycaps, fold creases numbered N/8, thumb-index tabs on the edge, and a rounded die-cut card corner, with everything inside square.

STORY: The visitor learns that nuntio is the terminal for AI agents, sees image paste happen, then reads panel by panel: images, agents, notes, settings, WSL and protocols. They believe it because every claim is a screenshot or an exact sequence, and they download.

FIRST VIEWPORT: A cover panel. The left half is an orange field with a huge condensed lowercase "nuntio", the hook "The terminal for AI agents.", a one-line offer and a black Download slab for the detected OS with file size. The right half holds Fig. 1, a two-pane terminal (claude | omp) demonstrating paste with numbered callouts. A ruled spec band runs along the bottom.

FORM: Reference card. Each panel head has fixed tiers: number and name, hook, then the exact key or setting. State is a mark (● ◐ ○), not a hue. Screenshots carry numbered callouts with a legend.

SIGNATURE: Press Ctrl+V with an image anywhere on the page. Fig. 1 types the nuntio temp path, turns it into [Image #N] and shows the image inline in the omp pane. Everything happens locally in the browser, and the figure has a static state without JS.
