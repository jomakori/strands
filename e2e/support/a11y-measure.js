// In-page measurement helpers for tests/a11y.spec.ts.
//
// axe-core cannot see two of the things ticket T18b asks for: a foreground/background pair
// (`color-contrast` comes back *incomplete* on every text node here because the app paints
// gradients) and the keyboard's focus ring. This script is injected into the page so both
// can be measured from the same colour maths, without duplicating a parser in every
// `page.evaluate` call.
(() => {
  const FOCUSABLE =
    "a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])";

  // ---- colour -----------------------------------------------------------------
  // Computed values arrive as oklch(), rgb()/rgba() or color(srgb ...), depending on how the
  // token was written, so all three are parsed and everything below works in sRGB.
  const clamp01 = (n) => Math.min(1, Math.max(0, n));

  function oklchToRgb(l, c, hDeg) {
    const h = (hDeg * Math.PI) / 180;
    const a = c * Math.cos(h);
    const b = c * Math.sin(h);
    const l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    const m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    const s_ = l - 0.0894841775 * a - 1.291485548 * b;
    const L = l_ ** 3;
    const M = m_ ** 3;
    const S = s_ ** 3;
    const lin = [
      4.0767416621 * L - 3.3077115913 * M + 0.2309699292 * S,
      -1.2684380046 * L + 2.6097574011 * M - 0.3413193965 * S,
      -0.0041960863 * L - 0.7034186147 * M + 1.707614701 * S,
    ];
    const gamma = (v) => (v <= 0.0031308 ? 12.92 * v : 1.055 * Math.pow(clamp01(v), 1 / 2.4) - 0.055);
    return lin.map((v) => Math.round(clamp01(gamma(v)) * 255));
  }

  function parseColor(input) {
    const value = (input || "").trim().toLowerCase();
    if (!value || value === "transparent") return { r: 0, g: 0, b: 0, a: 0 };
    const rgb = value.match(/rgba?\(([^)]+)\)/);
    if (rgb) {
      const parts = rgb[1].split(/[\s,/]+/).filter(Boolean).map(Number);
      return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
    }
    const oklch = value.match(/oklch\(([^)]+)\)/);
    if (oklch) {
      const [head, tail] = oklch[1].split("/");
      const [l, c, h] = head.trim().split(/\s+/).map(parseFloat);
      const [r, g, b] = oklchToRgb(l, c || 0, h || 0);
      return { r, g, b, a: tail ? clamp01(parseFloat(tail)) : 1 };
    }
    const srgb = value.match(/color\(srgb\s+([^)]+)\)/);
    if (srgb) {
      const parts = srgb[1].split(/[\s/]+/).filter(Boolean).map(Number);
      return {
        r: Math.round(clamp01(parts[0]) * 255),
        g: Math.round(clamp01(parts[1]) * 255),
        b: Math.round(clamp01(parts[2]) * 255),
        a: parts.length > 3 ? parts[3] : 1,
      };
    }
    return null;
  }

  const composite = (top, bottom) => ({
    r: top.r * top.a + bottom.r * (1 - top.a),
    g: top.g * top.a + bottom.g * (1 - top.a),
    b: top.b * top.a + bottom.b * (1 - top.a),
    a: 1,
  });

  const luminance = ({ r, g, b }) => {
    const lin = (v) => {
      const s = v / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  };

  const contrast = (a, b) => {
    const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
  };

  /** The painted backdrop behind `el`, built from its own background-color and its ancestors'. */
  function backgroundOf(el) {
    const layers = [];
    let node = el;
    while (node) {
      const colour = parseColor(getComputedStyle(node).backgroundColor);
      if (colour && colour.a > 0) {
        layers.push(colour);
        if (colour.a >= 1) break;
      }
      node = node.parentElement;
    }
    layers.push({ r: 255, g: 255, b: 255, a: 1 });
    return layers.reduceRight((bottom, top) => composite(top, bottom));
  }

  const fmt = (c) => `rgb(${Math.round(c.r)} ${Math.round(c.g)} ${Math.round(c.b)})`;

  // ---- dom reading --------------------------------------------------------------
  const pathOf = (el) => {
    const parts = [];
    let node = el;
    while (node && node !== document.body && parts.length < 4) {
      const cls = node.className && typeof node.className === "string" ? "." + node.className.trim().split(/\s+/).join(".") : "";
      parts.unshift(node.tagName.toLowerCase() + (node.id ? "#" + node.id : "") + cls);
      node = node.parentElement;
    }
    return parts.join(" > ");
  };

  const hidden = (el) => {
    if (el.closest("[aria-hidden='true']")) return true;
    const cs = getComputedStyle(el);
    return cs.visibility === "hidden" || cs.display === "none" || Number(cs.opacity) === 0;
  };

  const visible = (el) => {
    if (hidden(el)) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  const labelOf = (el) =>
    `${el.tagName.toLowerCase()}: ${(el.getAttribute("aria-label") || el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 40)}`;

  /**
   * The accessible name, the way ARIA computes it — deliberately without `placeholder`.
   * axe's `label` rule accepts a placeholder as a substitute, but a placeholder is not a
   * label: it disappears the moment the field has a value and cannot be styled for contrast.
   */
  function nameOf(el) {
    const text = (node) => (node.textContent || "").trim().replace(/\s+/g, " ");

    const labelledBy = el.getAttribute("aria-labelledby");
    if (labelledBy) {
      const parts = labelledBy
        .split(/\s+/)
        .map((id) => document.getElementById(id))
        .filter(Boolean)
        .map(text)
        .join(" ")
        .trim();
      if (parts) return parts;
    }

    const ariaLabel = el.getAttribute("aria-label");
    if (ariaLabel && ariaLabel.trim()) return ariaLabel.trim();

    if (el.id) {
      const explicit = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
      if (explicit && text(explicit)) return text(explicit);
    }

    const wrapping = el.closest("label");
    if (wrapping && text(wrapping)) return text(wrapping);

    if (el instanceof HTMLImageElement) {
      const alt = el.getAttribute("alt");
      if (alt && alt.trim()) return alt.trim();
    }

    if (el.tagName === "INPUT") {
      const type = (el.getAttribute("type") || "text").toLowerCase();
      if (type === "button" || type === "submit" || type === "reset") {
        const value = el.getAttribute("value");
        if (value && value.trim()) return value.trim();
      }
    }

    const title = el.getAttribute("title");
    if (title && title.trim()) return title.trim();

    // Text content only counts when the control's own name plausibly comes from it; a form
    // field's text content is never its label.
    if (!["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName)) {
      const own = text(el);
      if (own) return own;
    }
    return "";
  }

  const state = { domOrder: [], index: new WeakMap(), stops: [], recording: false };

  /** Snapshot the DOM tab order and start recording what actually receives focus. */
  function start() {
    state.domOrder = Array.from(document.querySelectorAll(FOCUSABLE))
      .filter(visible)
      .map((el, index) => {
        state.index.set(el, index);
        const rect = el.getBoundingClientRect();
        return {
          index,
          path: pathOf(el),
          label: labelOf(el),
          name: nameOf(el),
          top: Math.round(rect.top + window.scrollY),
          left: Math.round(rect.left + window.scrollX),
        };
      });

    state.stops = [];
    state.recording = true;
    document.addEventListener(
      "focusin",
      (event) => {
        if (!state.recording) return;
        const el = event.target;
        if (!(el instanceof HTMLElement)) return;
        const cs = getComputedStyle(el);
        const width = parseFloat(cs.outlineWidth) || 0;
        const outline = parseColor(cs.outlineColor);
        const hasOutline = cs.outlineStyle !== "none" && width > 0 && outline && outline.a > 0;
        // An outline paints offset from the box, so its backdrop is the parent surface, not
        // the control's own fill.
        const backdrop = backgroundOf(el.parentElement || el);
        let ring = null;
        let note = "";
        if (hasOutline && width >= 2) {
          const ratio = contrast(composite(outline, backdrop), backdrop);
          ring = ratio >= 3;
          note = `${width}px ${cs.outlineStyle} ${fmt(outline)} on ${fmt(backdrop)} = ${ratio.toFixed(2)}:1`;
        } else if (cs.boxShadow && cs.boxShadow !== "none") {
          ring = true;
          note = `box-shadow ${cs.boxShadow.slice(0, 60)}`;
        } else {
          ring = false;
          note = `no ring (outline ${cs.outlineStyle} ${width}px, box-shadow ${cs.boxShadow})`;
        }
        const path = pathOf(el);
        const label = labelOf(el);
        if (state.stops.some((stop) => stop.path === path && stop.label === label)) {
          // Tab has cycled back to the first control; the walk is complete.
          state.recording = false;
          return;
        }
        state.stops.push({
          index: state.index.has(el) ? state.index.get(el) : -1,
          path,
          label,
          ring,
          note,
        });
      },
      true,
    );
  }

  /** Contrast over every element that paints its own text. */
  function textSamples() {
    const samples = [];
    const failures = [];
    const seen = new Set();
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);

    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (!node.textContent || !node.textContent.trim()) continue;
      const el = node.parentElement;
      if (!el || seen.has(el) || !visible(el)) continue;
      if (el.closest("[disabled], option, [aria-hidden='true']")) continue;
      seen.add(el);

      const cs = getComputedStyle(el);
      const colour = parseColor(cs.color);
      if (!colour) continue;

      const backdrop = backgroundOf(el);
      const size = parseFloat(cs.fontSize);
      const bold = Number(cs.fontWeight) >= 700;
      const required = size >= 24 || (bold && size >= 18.66) ? 3 : 4.5;
      const ratio = Math.round(contrast(composite(colour, backdrop), backdrop) * 100) / 100;

      const sample = {
        target: pathOf(el),
        ratio,
        required,
        size,
        color: fmt(colour),
        background: fmt(backdrop),
        html: el.outerHTML.slice(0, 140),
      };
      samples.push(sample);
      if (ratio < required) failures.push(sample);
    }
    return { samples, failures };
  }

  /** Everything the keyboard pass measured. */
  function report() {
    state.recording = false;
    const { samples, failures } = textSamples();

    // Visual order: rows top-to-bottom, left-to-right inside a row.
    const rows = [];
    for (const item of [...state.domOrder].sort((a, b) => a.top - b.top || a.left - b.left)) {
      const row = rows.find((candidate) => Math.abs(candidate[0].top - item.top) <= 8);
      if (row) row.push(item);
      else rows.push([item]);
    }
    const visual = rows.flatMap((row) => row.sort((a, b) => a.left - b.left));
    const layoutJumps = visual
      .map((item, position) => (item.index === state.domOrder[position].index ? null : `visual #${position} is ${item.label} but DOM #${position} is ${state.domOrder[position].label}`))
      .filter(Boolean);

    // The keyboard's own order, allowing for the single wrap at the end of the cycle.
    const orderJumps = [];
    for (let i = 1; i < state.stops.length; i += 1) {
      if (state.stops[i].index < state.stops[i - 1].index) {
        // A wrap is only legitimate when every later stop is a new cycle start; anything else
        // is the focus order jumping backwards through the page.
        const rest = state.stops.slice(i);
        const wraps = rest.every((stop, offset) => offset === 0 || stop.index > rest[offset - 1].index);
        if (!wraps) orderJumps.push(`${state.stops[i - 1].label} → ${state.stops[i].label}`);
      }
    }

    const reached = new Set(state.stops.map((stop) => stop.path));
    return {
      samples: samples.length,
      failures,
      domCount: state.domOrder.length,
      stops: state.stops,
      missing: state.stops.filter((stop) => !stop.ring).map((stop) => `${stop.path} — ${stop.note}`),
      unreachable: state.domOrder.filter((item) => !reached.has(item.path)).map((item) => `${item.path} (${item.label})`),
      unnamed: state.domOrder.filter((item) => !item.name).map((item) => item.path),
      tabOrder: state.stops.map((stop) => `${stop.label} @${stop.index}`),
      orderJumps,
      layoutJumps,
    };
  }

  window.__a11y = { start, report, textSamples, domCount: () => state.domOrder.length };
})();
