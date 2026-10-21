// What Syndeo does in a page for an agent: read it, list what can be clicked
// or typed into, and click, type and scroll. It runs in Syndeo's own content
// world, so the page can neither see these functions nor change them, and it
// is the same code every time: an agent chooses what to do, never the code
// that does it. JavaScript of an agent's own runs only through eval, in the
// page's world, with the person's separate leave.
//
// Called as the body of an async function, with `op` and `a`, the request's
// arguments as JSON; it returns its answer as JSON.
const S = (globalThis.__syndeoAgent ??= (() => {
  // Elements are referred to as e<n>: n is the index of a weak reference,
  // kept for as long as the document is.
  const refs = [];
  const numbers = new WeakMap();
  const refOf = (el) => {
    let n = numbers.get(el);
    if (n === undefined) {
      n = refs.length;
      refs.push(new WeakRef(el));
      numbers.set(el, n);
    }
    return "e" + n;
  };
  const element = (ref) => {
    const m = /^e(\d+)$/.exec(String(ref));
    const el = m && refs[Number(m[1])] ? refs[Number(m[1])].deref() : undefined;
    if (!el || !el.isConnected) {
      throw new Error("no element " + ref + " on this page now; take a new snapshot");
    }
    return el;
  };
  const shown = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    const s = getComputedStyle(el);
    return s.visibility !== "hidden" && s.display !== "none";
  };
  const one = (text, n) => String(text || "").replace(/\s+/g, " ").trim().slice(0, n);
  const roleOf = (el) => {
    const role = el.getAttribute("role");
    if (role) return role;
    const tag = el.tagName;
    if (tag === "A") return "link";
    if (tag === "BUTTON" || tag === "SUMMARY") return "button";
    if (tag === "SELECT") return "combobox";
    if (tag === "TEXTAREA") return "textbox";
    if (tag === "INPUT") {
      const type = (el.type || "text").toLowerCase();
      if (["submit", "button", "reset", "image"].includes(type)) return "button";
      if (type === "checkbox" || type === "radio") return type;
      if (type === "range") return "slider";
      return "textbox";
    }
    return el.isContentEditable ? "textbox" : "generic";
  };
  const nameOf = (el) =>
    one(
      el.getAttribute("aria-label") ||
        (el.labels && el.labels.length ? el.labels[0].innerText : "") ||
        el.getAttribute("title") ||
        el.getAttribute("placeholder") ||
        el.getAttribute("alt") ||
        el.innerText ||
        (el.type === "password" ? "" : el.value),
      200
    );
  const QUERY =
    "a[href], button, input:not([type=hidden]), textarea, select, summary, " +
    "[role=button], [role=link], [role=checkbox], [role=radio], [role=tab], " +
    "[role=menuitem], [role=option], [role=textbox], [role=combobox], " +
    "[contenteditable=''], [contenteditable=true], [onclick]";
  const describe = () => ({
    url: location.href,
    title: document.title,
  });
  return {
    read(a) {
      const text = document.body ? document.body.innerText : "";
      return { ...describe(), text: text.slice(0, a.max), truncated: text.length > a.max };
    },
    snapshot(a) {
      const elements = [];
      let truncated = false;
      for (const el of document.querySelectorAll(QUERY)) {
        if (!shown(el)) continue;
        if (elements.length >= a.max) {
          truncated = true;
          break;
        }
        const role = roleOf(el);
        const e = { ref: refOf(el), role, name: nameOf(el) };
        if (el.tagName === "A" && el.href) e.href = String(el.href).slice(0, 500);
        // A password field's value is never read out.
        if (el.type === "password") e.secret = true;
        else if (role === "textbox" && typeof el.value === "string") e.value = el.value.slice(0, 200);
        if (el.tagName === "SELECT") e.value = el.value;
        if (role === "checkbox" || role === "radio") e.checked = !!el.checked;
        if (el.disabled) e.disabled = true;
        elements.push(e);
      }
      return { ...describe(), elements, truncated };
    },
    click(a) {
      const el = element(a.ref);
      el.scrollIntoView({ block: "center", inline: "center" });
      if (typeof el.focus === "function") el.focus({ preventScroll: true });
      el.click();
      return { clicked: a.ref };
    },
    type(a) {
      const el = element(a.ref);
      el.scrollIntoView({ block: "center", inline: "center" });
      if (typeof el.focus === "function") el.focus({ preventScroll: true });
      if (el.tagName === "SELECT") {
        const option = [...el.options].find((o) => o.value === a.text || o.label === a.text);
        if (!option) throw new Error("no option " + a.text);
        el.value = option.value;
      } else if (el.isContentEditable) {
        document.execCommand("selectAll", false);
        document.execCommand("insertText", false, a.text);
      } else if (el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
        // The element's own value setter, past any a page's framework adds.
        const proto = el.tagName === "INPUT" ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
        Object.getOwnPropertyDescriptor(proto, "value").set.call(el, a.text);
      } else {
        throw new Error(a.ref + " is not a field to type into");
      }
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      if (a.submit) {
        if (el.form) {
          if (typeof el.form.requestSubmit === "function") el.form.requestSubmit();
          else el.form.submit();
        } else {
          for (const type of ["keydown", "keypress", "keyup"]) {
            el.dispatchEvent(
              new KeyboardEvent(type, { key: "Enter", code: "Enter", keyCode: 13, which: 13, bubbles: true, cancelable: true })
            );
          }
        }
      }
      return { typed: a.ref };
    },
    scroll(a) {
      if (a.ref) element(a.ref).scrollIntoView({ block: "center", inline: "center" });
      else window.scrollBy(a.dx || 0, a.dy || 0);
      return { x: window.scrollX, y: window.scrollY };
    },
  };
})());
if (!Object.prototype.hasOwnProperty.call(S, op)) throw new Error("no such operation");
return JSON.stringify(S[op](JSON.parse(a)));
