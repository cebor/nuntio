// Home page enhancements. Everything works without this file.

// Point the cover's download button and the download tabs at the visitor's OS.
(() => {
  const ua = navigator.userAgent;
  // iPhones and iPads (which claim to be a Mac) get no macOS download.
  const mobile = /iPhone|iPad|iPod/.test(ua) || navigator.maxTouchPoints > 1;
  const os = /Mac/.test(ua) && !mobile ? "mac"
    : /Win/.test(ua) ? "windows"
    : /Linux|X11/.test(ua) && !/Android/.test(ua) ? "linux"
    : null;
  if (!os) return;
  const tab = document.getElementById("dl-" + os);
  if (!tab) return;
  tab.checked = true;
  const primary = document.querySelector(`.dl-panel[data-os="${os}"] a.primary`);
  const hero = document.getElementById("hero-download");
  if (!primary || !hero) return;
  hero.href = primary.href;
  const names = { mac: "macOS", linux: "Linux", windows: "Windows" };
  const label = hero.querySelector("[data-hero-label]");
  if (label) label.textContent = "Download for " + names[os];
  const meta = hero.querySelector("[data-hero-meta]");
  const fileLabel = primary.querySelector(".file-label");
  const fileMeta = primary.querySelector(".file-meta");
  if (meta && fileLabel && fileMeta) {
    const parts = fileMeta.textContent.split(" · ");
    meta.textContent = fileLabel.textContent.trim() + " · " + parts[parts.length - 1].trim();
  }
})();

// Fig. 1: paste an image anywhere on the page and watch nuntio's image paste.
// The image stays in the browser: it becomes an object URL and nothing else.
(() => {
  const fig = document.getElementById("paste-demo");
  if (!fig) return;
  const path = fig.querySelector("[data-demo-path]");
  const attach = fig.querySelector("[data-demo-attach]");
  const name = fig.querySelector("[data-demo-name]");
  const image = fig.querySelector("img[data-demo-image]");
  const live = fig.querySelector("[data-demo-live]");
  const reduce = matchMedia("(prefers-reduced-motion: reduce)");
  let n = 0;
  let url = null;
  let timers = [];

  document.addEventListener("paste", (event) => {
    const items = event.clipboardData ? Array.from(event.clipboardData.items) : [];
    const item = items.find((i) => i.kind === "file" && i.type.startsWith("image/"));
    const file = item && item.getAsFile();
    if (!file) return;
    event.preventDefault();
    n += 1;
    if (url) URL.revokeObjectURL(url);
    url = URL.createObjectURL(file);
    path.textContent = `/tmp/nuntio-paste-48213-1760531207118/image-${n}.png`;
    attach.textContent = `[Image #${n}]`;
    name.textContent = `image-${n}.png`;
    image.src = url;
    live.textContent = `Saved as image-${n}.png and attached as [Image #${n}].`;

    timers.forEach(clearTimeout);
    timers = [];
    if (reduce.matches) {
      fig.dataset.state = "done";
      return;
    }
    fig.dataset.state = "path";
    timers.push(setTimeout(() => { fig.dataset.state = "attached"; }, 600));
    timers.push(setTimeout(() => { fig.dataset.state = "done"; }, 1100));
  });
})();

// Thumb index: mark the tab of the panel in the middle of the viewport.
(() => {
  const links = new Map();
  document.querySelectorAll(".thumb-index a[href^='#']").forEach((a) => {
    links.set(a.getAttribute("href").slice(1), a);
  });
  if (!links.size || !("IntersectionObserver" in window)) return;
  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      links.forEach((a, id) => {
        if (id === entry.target.id) a.setAttribute("aria-current", "true");
        else a.removeAttribute("aria-current");
      });
    }
  }, { rootMargin: "-45% 0px -50% 0px" });
  document.querySelectorAll("section.panel[id]").forEach((s) => observer.observe(s));
  // The cover has no tab: clear the mark when it is back in the middle.
  const cover = document.querySelector("section.cover");
  if (cover) {
    new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) links.forEach((a) => a.removeAttribute("aria-current"));
    }, { rootMargin: "-45% 0px -50% 0px" }).observe(cover);
  }
})();
