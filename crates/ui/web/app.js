// card-ledger window. Catalogue and Collectr text is only ever set with textContent.
"use strict";

const MARKER = "X-Card-Ledger";
const IMAGE_HOST = "https://assets.tcgdex.net/";
const CONDS = ["unchecked", "NM", "LP", "MP", "HP", "DMG"];
const COND_NAMES = {
  unchecked: "Unchecked", NM: "Near Mint", LP: "Lightly Played",
  MP: "Moderately Played", HP: "Heavily Played", DMG: "Damaged",
};
const PILES = [
  ["toploader", "Toploader"],
  ["small-binder", "Small binder"],
  ["big-binder", "Big binder"],
  ["box", "Box"],
  ["set-aside", "Set aside"],
  ["not-in-export", "Not in export"],
];
const PILE_NAME = Object.fromEntries(PILES);
const PILE_RANK = Object.fromEntries(PILES.map(([k], i) => [k, i]));
const FLAG_NAMES = {
  VariantNotListed: "Variant not listed",
  FirstEditionCheck: "1st Edition or Unlimited",
  BaseSetLookalike: "Base Set lookalike",
  PromoVsSet: "Promo or set card",
  RadiantNumber: "Radiant Collection number",
  Japanese: "Japanese",
  NotPokemon: "Not Pokémon",
  PriceDisagrees: "Price disagrees",
  NameMismatch: "Name mismatch",
  NoPrice: "No price",
};
const TOGGLES = [
  ["unchecked", "Unchecked", (c) => c.condition === "unchecked"],
  ["flagged", "Flagged", (c) => c.flags.length > 0],
  ["check", "Check condition", (c) => c.check_condition],
  ["unresolved", "Unresolved", (c) => !c.canonical],
  ["notpulled", "Not pulled", (c) => !c.pulled],
];

const $ = (id) => document.getElementById(id);
const state = { view: null, pile: null, on: new Set(), q: "", sort: "value", saving: new Set(), loadSeq: 0 };

// ---------- formatting ----------

function money(v) {
  if (v === null || v === undefined || !Number.isFinite(v)) return "no price";
  return "$" + v.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

function signedMoney(v) {
  if (!Number.isFinite(v)) return "";
  return (v < 0 ? "−" : "+") + money(Math.abs(v));
}

function range(lo, hi) {
  if (lo == null || hi == null) return "";
  if (Math.abs(hi - lo) < 0.005) return money(lo);
  return money(lo) + "–" + money(hi);
}

// Dates come as "2026-11-24" or an ISO time. Shown in local time, 12-hour clock.
function when(s) {
  if (!s) return "";
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (d) {
    return new Date(+d[1], +d[2] - 1, +d[3]).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
  }
  const t = new Date(s);
  if (Number.isNaN(t.getTime())) return s;
  return t.toLocaleString("en-US", { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" });
}

function plural(n, one, many) {
  return n + " " + (n === 1 ? one : many || one + "s");
}

// ---------- DOM helpers (text only, never markup) ----------

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined && text !== null) e.textContent = String(text);
  return e;
}

function add(parent, ...kids) {
  for (const k of kids) if (k) parent.append(k);
  return parent;
}

function pileClass(p) {
  return PILE_NAME[p] ? "p-" + p : "p-box";
}

function setStatus(text, isError, keep) {
  const s = $("status");
  s.hidden = !text;
  s.textContent = text || "";
  s.classList.toggle("error", !!isError);
  clearTimeout(setStatus.timer);
  // Confirmations clear themselves; errors stay until the next message or a tap.
  if (text && !isError && !keep) setStatus.timer = setTimeout(() => { s.hidden = true; }, 5000);
}

// ---------- server ----------

async function api(path, body) {
  const opts = body === undefined
    ? { headers: { Accept: "application/json" } }
    : { method: "POST", headers: { "Content-Type": "application/json", [MARKER]: "1" }, body: JSON.stringify(body) };
  const r = await fetch(path, opts);
  let data = null;
  try { data = await r.json(); } catch (_) { /* not JSON */ }
  if (!r.ok) throw new Error((data && data.error) || "the window's server answered " + r.status);
  return data;
}

async function load() {
  const seq = ++state.loadSeq;
  const v = await api("/api/ledger");
  if (seq !== state.loadSeq) return; // a newer load is on its way
  state.view = v;
  render();
}

// Re-fetches the ledger and redraws without moving the page or losing the focused control.
async function reload() {
  const y = window.scrollY;
  const a = document.activeElement;
  const row = a && a.closest ? a.closest(".row") : null;
  const focus = row ? { copy: row.dataset.copy, key: a.dataset.key } : null;
  await load();
  window.scrollTo(0, y);
  if (focus && focus.key) {
    const r = document.querySelector('.row[data-copy="' + focus.copy + '"]');
    const t = r && r.querySelector('[data-key="' + focus.key + '"]');
    if (t) t.focus({ preventScroll: true });
  }
}

function cardTitle(c) {
  return c.catalog_name || c.product_name;
}

async function setCondition(c, cond) {
  if (state.saving.has(c.copy_id) || c.condition === cond) return;
  state.saving.add(c.copy_id);
  markSaving(c.copy_id, true);
  try {
    await api("/api/copy/" + c.copy_id + "/condition", { condition: cond });
    setStatus(cardTitle(c) + ": condition set to " + COND_NAMES[cond] + ".");
  } catch (e) {
    setStatus("Not saved: " + e.message, true);
  } finally {
    state.saving.delete(c.copy_id);
  }
  await reloadQuietly(c.copy_id);
}

async function setPulled(c, pulled) {
  if (state.saving.has(c.copy_id)) return;
  state.saving.add(c.copy_id);
  markSaving(c.copy_id, true);
  try {
    await api("/api/copy/" + c.copy_id + "/pulled", { pulled });
    setStatus(cardTitle(c) + (pulled ? ": pulled." : ": not pulled."));
  } catch (e) {
    setStatus("Not saved: " + e.message, true);
  } finally {
    state.saving.delete(c.copy_id);
  }
  await reloadQuietly(c.copy_id);
}

async function reloadQuietly(id) {
  try {
    await reload();
  } catch (e) {
    setStatus("Couldn't reload the ledger: " + e.message, true);
    markSaving(id, false);
  }
}

function markSaving(id, on) {
  const r = document.querySelector('.row[data-copy="' + id + '"]');
  if (!r) return;
  r.classList.toggle("saving", on);
  // Not disabled: that would drop keyboard focus. state.saving ignores repeat clicks.
  r.setAttribute("aria-busy", String(on));
}

async function refreshPrices() {
  const b = $("refresh");
  b.disabled = true;
  b.textContent = "Refreshing…";
  setStatus("Refreshing prices…", false, true);
  try {
    const r = await api("/api/refresh", {});
    setStatus(r.report || "Prices refreshed.", false, true);
    await reload();
  } catch (e) {
    setStatus(e.message, true);
  } finally {
    b.disabled = false;
    b.textContent = "Refresh prices";
  }
}

// ---------- filtering ----------

function matches(c) {
  if (state.pile && c.pile !== state.pile) return false;
  for (const [k, , f] of TOGGLES) if (state.on.has(k) && !f(c)) return false;
  const q = state.q;
  if (q) {
    const hay = [c.product_name, c.catalog_name, c.set_name, c.card_number, c.canonical, c.variant, c.rarity, c.note]
      .filter(Boolean).join(" ").toLowerCase();
    for (const w of q.split(/\s+/)) if (w && !hay.includes(w.replace(/^#/, ""))) return false;
  }
  return true;
}

function sorted(cards) {
  const by = {
    value: (a, b) => (b.value ?? -1) - (a.value ?? -1),
    // `order` keys are built to compare byte-wise (zero-padded numbers, "~" after digits for cards
    // with no Pokédex number); localeCompare would put "~" first.
    pile: (a, b) => (PILE_RANK[a.pile] ?? 9) - (PILE_RANK[b.pile] ?? 9) || (a.order < b.order ? -1 : a.order > b.order ? 1 : 0),
    set: (a, b) => a.set_name.localeCompare(b.set_name) || a.card_number.localeCompare(b.card_number, "en", { numeric: true }),
    name: (a, b) => cardTitle(a).localeCompare(cardTitle(b)),
  }[state.sort] || (() => 0);
  return cards.slice().sort((a, b) => by(a, b) || a.copy_id - b.copy_id);
}

// ---------- rendering ----------

function render() {
  const v = state.view;
  if (!v) return;
  renderSummary(v);
  renderPiles(v);
  renderToggles(v);
  renderList(v);
}

// Only ever the last four characters of the key reach the window (the server cuts it).
function conditionKeyText(j) {
  if (!j || !j.on) return j && j.problem ? "off (" + j.problem + ")" : "off";
  return j.ends ? "on, key ends …" + j.ends : "on";
}

function renderSummary(v) {
  const parts = [plural(v.total_cards, "card")];
  if (v.collectr_price_date) parts.push("Collectr export priced " + when(v.collectr_price_date));
  $("sub").textContent = parts.join(" · ");
  $("s-value").textContent = money(v.total_value_unchecked_lp);
  $("s-value-nm").textContent = money(v.total_value);
  $("s-collectr").textContent = money(v.collectr_total);
  $("s-diff").textContent = v.collectr_total ? signedMoney(v.total_value_unchecked_lp - v.collectr_total) + " vs Collectr (NM)" : "";
  $("s-unchecked").textContent = v.unchecked;
  $("s-unchecked-of").textContent = "of " + plural(v.total_cards, "card");
  $("s-unresolved").textContent = v.unresolved;
  $("s-flagged").textContent = v.flagged;
  $("s-fetched").textContent = v.prices_fetched_at ? "Fetched " + when(v.prices_fetched_at) : "No prices fetched yet";
  $("s-collectr-date").textContent = v.collectr_price_date ? "Collectr figures from " + when(v.collectr_price_date) : "";
  $("s-justtcg").textContent = "Condition prices (JustTCG): " + conditionKeyText(v.justtcg);
}

function renderPiles(v) {
  const totals = Object.fromEntries((v.piles || []).map((p) => [p.pile, p]));
  const keys = PILES.map(([k]) => k);
  for (const p of v.piles || []) if (!keys.includes(p.pile)) keys.push(p.pile);
  const box = $("piles");
  box.replaceChildren();
  for (const k of keys) {
    const t = totals[k] || { cards: 0, pulled: 0, value: 0 };
    // The not-in-export group only appears when a re-import left cards behind.
    if (k === "not-in-export" && !t.cards) continue;
    const b = el("button", "pile " + pileClass(k));
    b.type = "button";
    b.setAttribute("aria-pressed", String(state.pile === k));
    const bar = el("span", "bar");
    bar.setAttribute("aria-hidden", "true");
    const fill = el("i");
    fill.style.width = (t.cards ? Math.round((t.pulled / t.cards) * 100) : 0) + "%";
    bar.append(fill);
    const stats = add(el("span", "pstats"), el("span", "pcount", t.cards), el("span", "pval", money(t.value)));
    add(b, el("span", "pname", PILE_NAME[k] || k), stats, bar, el("span", "ppulled", t.pulled + " of " + t.cards + " pulled"));
    b.addEventListener("click", () => {
      state.pile = state.pile === k ? null : k;
      // The big binder is filed in Pokédex order, so show it that way.
      if (state.pile === "big-binder") {
        state.sort = "pile";
        $("sort").value = "pile";
      }
      render();
    });
    box.append(b);
  }
}

function renderToggles(v) {
  const box = $("toggles");
  box.replaceChildren();
  for (const [k, label, f] of TOGGLES) {
    const b = el("button", "toggle");
    b.type = "button";
    b.setAttribute("aria-pressed", String(state.on.has(k)));
    add(b, el("span", null, label), el("span", "n", v.cards.filter(f).length));
    b.addEventListener("click", () => {
      if (state.on.has(k)) state.on.delete(k); else state.on.add(k);
      render();
    });
    box.append(b);
  }
}

function renderList(v) {
  const shown = sorted(v.cards.filter(matches));
  // Same basis as the headline and the pile tiles: unchecked cards at their LP value.
  // Set-aside and not-in-export cards are left out, as in the headline total.
  const value = shown
    .filter((c) => c.pile !== "set-aside" && c.pile !== "not-in-export")
    .reduce((a, c) => a + (c.value_counted || 0), 0);
  const filtered = shown.length !== v.cards.length;
  $("count").textContent = (filtered ? "Showing " + shown.length + " of " + plural(v.cards.length, "card") : plural(shown.length, "card"))
    + " · " + money(value) + (state.pile ? " · " + (PILE_NAME[state.pile] || state.pile) + " only" : "");
  const list = $("list");
  list.replaceChildren();
  if (!shown.length) {
    list.append(el("li", "empty", v.cards.length ? "No cards match these filters." : "The ledger is empty. Import a Collectr export with: card-ledger import <file.csv>"));
    return;
  }
  const frag = document.createDocumentFragment();
  for (const c of shown) frag.append(row(c));
  list.append(frag);
}

function row(c) {
  const li = el("li", "row " + pileClass(c.pile));
  li.dataset.copy = c.copy_id;
  if (c.pulled) li.classList.add("pulled");

  // Image
  const thumb = el("div", "thumb");
  if (typeof c.image === "string" && c.image.startsWith(IMAGE_HOST)) {
    const img = document.createElement("img");
    img.loading = "lazy";
    img.decoding = "async";
    img.alt = "";
    img.width = 64;
    img.height = 89;
    img.src = c.image + "/low.webp";
    img.addEventListener("error", () => img.remove());
    thumb.append(img);
  } else {
    li.classList.add("noimg");
  }

  // Name, set, number, variant, identity
  const head = el("div", "headline");
  const title = cardTitle(c);
  add(head, el("h3", "name", title));
  const meta = [c.set_name, "#" + c.card_number, c.variant];
  if (c.rarity) meta.push(c.rarity);
  if (c.catalog_name && c.catalog_name !== c.product_name) meta.push("Collectr: " + c.product_name);
  if (c.copies_of_item > 1) meta.push("copy " + c.copy_no + " of " + c.copies_of_item);
  head.append(el("p", "meta", meta.filter(Boolean).join(" · ")));
  if (c.canonical) {
    add(head, add(el("p", "ident"), el("code", null, c.canonical)));
  } else {
    head.append(el("p", "ident unresolved", "Unresolved: " + (c.unresolved_reason || "no reason given")));
  }

  // Value, pile, pulled tick
  const side = el("div", "side");
  const vals = add(el("div", "vals"), el("span", "val", money(c.value)));
  const r = range(c.value_low, c.value_high);
  if (r) vals.append(el("span", "range", r));
  const tag = el("span", "tag", PILE_NAME[c.pile] || c.pile);
  const tick = el("label", "tick" + (c.pulled ? " on" : ""));
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = !!c.pulled;
  box.dataset.key = "pulled";
  box.setAttribute("aria-label", "Pulled: " + title);
  box.addEventListener("change", () => {
    if (state.saving.has(c.copy_id)) { box.checked = !box.checked; return; }
    setPulled(c, box.checked);
  });
  add(tick, box, el("span", null, "Pulled"));
  add(side, vals, tag, tick);

  // Badges, picker, prices, flags, research
  const main = el("div", "main");
  const chips = el("div", "chips");
  chips.append(el("span", "chip pilechip", (PILE_NAME[c.pile] || c.pile) + ": " + (c.pile_reason || "")));
  if (c.check_condition) chips.append(el("span", "chip check", "Check condition"));
  for (const f of c.flags) chips.append(el("span", "chip flag", FLAG_NAMES[f.code] || String(f.code)));
  main.append(chips);

  const picker = el("div", "picker");
  picker.setAttribute("role", "group");
  picker.setAttribute("aria-label", "Condition of " + title);
  CONDS.forEach((k, i) => {
    const b = el("button", "cond");
    b.type = "button";
    b.dataset.key = "cond-" + k;
    b.setAttribute("aria-pressed", String(c.condition === k));
    const price = i === 0 ? "as NM" : money(c.by_condition[i - 1]);
    b.title = COND_NAMES[k] + (i === 0 ? ": sorted by its NM value, counted at LP" : ": " + price);
    b.setAttribute("aria-label", COND_NAMES[k] + ", " + (i === 0 ? "sorted by NM value, counted at LP" : price));
    add(b, el("span", "c", k === "unchecked" ? "Unchecked" : k), el("span", "v", price));
    b.addEventListener("click", () => setCondition(c, k));
    picker.append(b);
  });
  main.append(picker);

  const dl = el("dl", "prices");
  const line = (label, amt, src) => {
    const dd = add(el("dd"), el("span", "amt", amt));
    if (src) dd.append(" ", el("span", "src", src));
    return add(el("div"), el("dt", null, label), dd);
  };
  add(dl,
    line("Value", money(c.value) + (r ? " (" + r + ")" : ""), c.value_basis),
    line("NM", money(c.nm_price), c.nm_label || ""),
    line("Collectr (NM)", money(c.collectr_price), c.collectr_price_date ? when(c.collectr_price_date) : ""));
  main.append(dl);

  const withMsg = c.flags.filter((f) => f.message);
  if (withMsg.length) {
    const ul = el("ul", "flags");
    for (const f of withMsg) ul.append(el("li", null, f.message));
    main.append(ul);
  }

  if (c.research) {
    const rs = c.research;
    const box2 = el("div", "research");
    box2.append(el("p", "rh", "Research" + (rs.confidence ? " · confidence: " + rs.confidence : "")));
    if (rs.check) box2.append(el("p", null, "Check: " + rs.check));
    if (rs.grade) box2.append(el("p", null, "Grading: " + rs.grade));
    if (rs.note) box2.append(el("p", null, rs.note));
    main.append(box2);
  }
  if (c.note) main.append(el("p", "note", "Note: " + c.note));

  add(li, thumb, head, side, main);
  return li;
}

// ---------- start ----------

function wire() {
  $("q").addEventListener("input", (e) => {
    state.q = e.target.value.trim().toLowerCase();
    if (state.view) renderList(state.view);
  });
  $("sort").addEventListener("change", (e) => {
    state.sort = e.target.value;
    if (state.view) renderList(state.view);
  });
  $("clear").addEventListener("click", () => {
    state.pile = null;
    state.on.clear();
    state.q = "";
    $("q").value = "";
    render();
  });
  $("refresh").addEventListener("click", refreshPrices);
  $("status").addEventListener("click", () => { $("status").hidden = true; });
}

// A view can be opened from the address: #pile=toploader&q=xy150&sort=pile&on=unchecked,flagged
function fromHash() {
  const h = new URLSearchParams(location.hash.replace(/^#/, ""));
  const pile = h.get("pile");
  if (pile && PILE_NAME[pile]) state.pile = pile;
  const q = (h.get("q") || "").trim();
  if (q) {
    state.q = q.toLowerCase();
    $("q").value = q;
  }
  const sort = h.get("sort");
  if (sort && [...$("sort").options].some((o) => o.value === sort)) {
    state.sort = sort;
    $("sort").value = sort;
  } else if (state.pile === "big-binder") {
    state.sort = "pile";
    $("sort").value = "pile";
  }
  const known = new Set(TOGGLES.map(([k]) => k));
  for (const k of (h.get("on") || "").split(",")) if (known.has(k)) state.on.add(k);
}

wire();
fromHash();
load().catch((e) => {
  $("sub").textContent = "The ledger didn't load.";
  setStatus("Couldn't load the ledger: " + e.message, true);
});
