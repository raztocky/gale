// Brickle's DOM and window API, built on the renderer's native DOM
// primitives (`dom`, see src/js.rs). Evaluates to a setup function that
// installs the API on the global object and returns the hooks the renderer
// calls. Everything here runs inside the sandboxed renderer.
(function (dom) {
  'use strict';
  const G = globalThis;
  const ids = new WeakMap();        // wrapper -> node id
  const wrappers = new Map();       // node id -> wrapper
  const listeners = new WeakMap();  // target -> Map(type -> [{fn, capture, once}])
  const handlers = new WeakMap();   // target -> {click: fn, ...} (onclick etc.)
  const TOKEN = Symbol('internal');
  let currentScript = null;
  let viewport = [1000, 700];

  const illegal = () => { throw new TypeError('Illegal constructor'); };
  const idOf = (n) => {
    const id = ids.get(n);
    if (id === undefined) throw new TypeError('Argument is not a Node');
    return id;
  };
  const str = (v) => String(v);

  function wrap(id) {
    if (id === -1 || id === null || id === undefined) return null;
    let w = wrappers.get(id);
    if (w) return w;
    switch (dom.nodeType(id)) {
      case 9: return document;
      case 3: w = new Text(TOKEN); break;
      case 11: w = new DocumentFragment(TOKEN); break;
      default: w = new HTMLElement(TOKEN);
    }
    ids.set(w, id);
    wrappers.set(id, w);
    return w;
  }
  const wrapAll = (list) => Array.from(list, wrap);
  // Accepts nodes and strings (for append/prepend/before/after).
  const toNode = (x) => (x instanceof Node ? x : document.createTextNode(str(x)));
  const elementsOnly = (list) => list.filter((n) => n.nodeType === 1);

  // ------------------------------------------------------------------ events
  class Event {
    constructor(type, init = {}) {
      this.type = str(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this.timeStamp = performance.now();
      this.isTrusted = false;
      this._stop = false;
      this._stopNow = false;
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() { this._stop = true; }
    stopImmediatePropagation() { this._stop = true; this._stopNow = true; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
    composedPath() { return this._path ? this._path.slice() : []; }
  }
  class CustomEvent extends Event {
    constructor(type, init = {}) { super(type, init); this.detail = init.detail === undefined ? null : init.detail; }
  }
  class UIEvent extends Event {}
  class MouseEvent extends UIEvent {
    constructor(type, init = {}) {
      super(type, init);
      for (const k of ['clientX', 'clientY', 'pageX', 'pageY', 'screenX', 'screenY', 'button', 'buttons'])
        this[k] = init[k] || 0;
      for (const k of ['ctrlKey', 'shiftKey', 'altKey', 'metaKey']) this[k] = !!init[k];
    }
  }
  class KeyboardEvent extends UIEvent {}
  class FocusEvent extends UIEvent {}

  const eventTargetMethods = {
    addEventListener(type, fn, opts) {
      if (typeof fn !== 'function' && !(fn && typeof fn.handleEvent === 'function')) return;
      const capture = typeof opts === 'boolean' ? opts : !!(opts && opts.capture);
      const once = !!(opts && typeof opts === 'object' && opts.once);
      let m = listeners.get(this);
      if (!m) listeners.set(this, (m = new Map()));
      const list = m.get(type) || [];
      if (!list.some((l) => l.fn === fn && l.capture === capture)) list.push({ fn, capture, once });
      m.set(type, list);
    },
    removeEventListener(type, fn, opts) {
      const capture = typeof opts === 'boolean' ? opts : !!(opts && opts.capture);
      const m = listeners.get(this);
      if (!m || !m.has(type)) return;
      m.set(type, m.get(type).filter((l) => !(l.fn === fn && l.capture === capture)));
    },
    dispatchEvent(ev) { return dispatchOn(this, ev); },
  };

  function invoke(target, ev, phase) {
    ev.currentTarget = target;
    ev.eventPhase = phase;
    const m = listeners.get(target);
    const list = m && m.get(ev.type);
    if (list) {
      for (const l of list.slice()) {
        if (phase === 1 && !l.capture) continue;
        if (phase === 3 && l.capture) continue;
        if (l.once) target.removeEventListener(ev.type, l.fn, l.capture);
        try {
          const r = typeof l.fn === 'function' ? l.fn.call(target, ev) : l.fn.handleEvent(ev);
          if (r === false) ev.preventDefault();
        } catch (e) { reportError(e); }
        if (ev._stopNow) return;
      }
    }
    if (phase !== 1) {
      const h = handlerFor(target, ev.type);
      if (h) {
        try { if (h.call(target, ev) === false) ev.preventDefault(); } catch (e) { reportError(e); }
      }
    }
  }

  function dispatchOn(target, ev) {
    ev.target = target;
    const path = [];
    if (target instanceof Node) {
      for (let n = target.parentNode; n; n = n.parentNode) path.push(n);
      if (path.length && path[path.length - 1] === document) path.push(G);
    }
    ev._path = [target, ...path];
    for (let i = path.length - 1; i >= 0 && !ev._stop; i--) invoke(path[i], ev, 1);
    if (!ev._stop) invoke(target, ev, 2);
    if (ev.bubbles) for (let i = 0; i < path.length && !ev._stop; i++) invoke(path[i], ev, 3);
    ev.currentTarget = null;
    ev.eventPhase = 0;
    return !ev.defaultPrevented;
  }

  // on<event> properties and attributes.
  const HANDLER_NAMES = ['click', 'dblclick', 'mousedown', 'mouseup', 'mouseover', 'mouseout', 'mouseenter',
    'mouseleave', 'keydown', 'keyup', 'keypress', 'input', 'change', 'submit', 'focus', 'blur', 'load', 'error',
    'scroll', 'resize', 'toggle'];
  function handlerFor(target, type) {
    const own = handlers.get(target);
    if (own && typeof own[type] === 'function') return own[type];
    if (target instanceof Element) {
      const code = target.getAttribute('on' + type);
      if (code !== null) {
        try {
          // eslint-disable-next-line no-new-func
          return new Function('event', code);
        } catch (e) { reportError(e); }
      }
    }
    return null;
  }
  function defineHandlers(proto) {
    for (const name of HANDLER_NAMES) {
      Object.defineProperty(proto, 'on' + name, {
        get() { const h = handlers.get(this); return (h && h[name]) || null; },
        set(fn) {
          let h = handlers.get(this);
          if (!h) handlers.set(this, (h = {}));
          h[name] = typeof fn === 'function' ? fn : null;
        },
        configurable: true,
      });
    }
  }

  function reportError(e) {
    const msg = e && e.stack ? e.stack : (e && e.message) || String(e);
    dom.log('error', msg);
    try {
      const ev = new Event('error');
      ev.message = e && e.message; ev.error = e;
      invoke(G, ev, 2);
    } catch (_) { /* ignore */ }
  }

  // ------------------------------------------------------------------- nodes
  class EventTarget {}
  Object.assign(EventTarget.prototype, eventTargetMethods);

  class Node extends EventTarget {
    constructor(token) { super(); if (token !== TOKEN) illegal(); }
    get nodeType() { return dom.nodeType(idOf(this)); }
    get nodeName() {
      switch (this.nodeType) {
        case 3: return '#text';
        case 9: return '#document';
        case 11: return '#document-fragment';
        default: return dom.tag(idOf(this)).toUpperCase();
      }
    }
    get parentNode() { return wrap(dom.parent(idOf(this))); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get childNodes() { return wrapAll(dom.children(idOf(this))); }
    get firstChild() { const c = dom.children(idOf(this)); return c.length ? wrap(c[0]) : null; }
    get lastChild() { const c = dom.children(idOf(this)); return c.length ? wrap(c[c.length - 1]) : null; }
    _sibling(delta, elementOnly) {
      const p = dom.parent(idOf(this));
      if (p === -1) return null;
      const c = dom.children(p);
      for (let i = c.indexOf(idOf(this)) + delta; i >= 0 && i < c.length; i += delta) {
        if (!elementOnly || dom.nodeType(c[i]) === 1) return wrap(c[i]);
      }
      return null;
    }
    get nextSibling() { return this._sibling(1, false); }
    get previousSibling() { return this._sibling(-1, false); }
    get isConnected() { return dom.connected(idOf(this)); }
    get ownerDocument() { return this === document ? null : document; }
    get textContent() { return this.nodeType === 9 ? null : dom.textContent(idOf(this)); }
    set textContent(v) { dom.setTextContent(idOf(this), v === null ? '' : str(v)); }
    get nodeValue() { return this.nodeType === 3 ? dom.textContent(idOf(this)) : null; }
    set nodeValue(v) { if (this.nodeType === 3) dom.setTextContent(idOf(this), str(v)); }
    hasChildNodes() { return dom.children(idOf(this)).length > 0; }
    appendChild(c) { dom.insert(idOf(this), idOf(c), null); return c; }
    insertBefore(c, ref) { dom.insert(idOf(this), idOf(c), ref ? idOf(ref) : null); return c; }
    removeChild(c) {
      if (dom.parent(idOf(c)) !== idOf(this)) throw new Error('NotFoundError: not a child of this node');
      dom.remove(idOf(c)); return c;
    }
    replaceChild(n, old) { this.insertBefore(n, old); return this.removeChild(old); }
    cloneNode(deep) { return wrap(dom.clone(idOf(this), !!deep)); }
    contains(other) {
      if (!other) return false;
      for (let n = other; n; n = n.parentNode) if (n === this) return true;
      return false;
    }
    getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
    isSameNode(o) { return o === this; }
    compareDocumentPosition(o) { return this === o ? 0 : this.contains(o) ? 20 : o.contains(this) ? 10 : 4; }
  }
  Node.ELEMENT_NODE = 1; Node.TEXT_NODE = 3; Node.DOCUMENT_NODE = 9; Node.DOCUMENT_FRAGMENT_NODE = 11;
  Node.COMMENT_NODE = 8;

  class CharacterData extends Node {
    get data() { return dom.textContent(idOf(this)); }
    set data(v) { dom.setTextContent(idOf(this), str(v)); }
    get length() { return this.data.length; }
    remove() { dom.remove(idOf(this)); }
  }
  class Text extends CharacterData {
    get wholeText() { return this.data; }
  }
  class Comment extends CharacterData {}

  // Methods shared by Element, Document and DocumentFragment.
  const parentNodeMethods = {
    get children() { return elementsOnly(this.childNodes); },
    get childElementCount() { return this.children.length; },
    get firstElementChild() { return this.children[0] || null; },
    get lastElementChild() { const c = this.children; return c[c.length - 1] || null; },
    querySelector(sel) { const r = dom.query(idOf(this), str(sel), false); return r.length ? wrap(r[0]) : null; },
    querySelectorAll(sel) { return nodeList(wrapAll(dom.query(idOf(this), str(sel), true))); },
    getElementsByTagName(tag) {
      tag = str(tag).toLowerCase();
      if (tag !== '*') return nodeList(wrapAll(dom.query(idOf(this), tag, true)));
      const out = [];
      const walk = (id) => { for (const c of dom.children(id)) if (dom.nodeType(c) === 1) { out.push(wrap(c)); walk(c); } };
      walk(idOf(this));
      return nodeList(out);
    },
    getElementsByClassName(names) {
      const sel = str(names).trim().split(/\s+/).filter(Boolean).map((c) => '.' + c).join('');
      return sel ? nodeList(wrapAll(dom.query(idOf(this), sel, true))) : nodeList([]);
    },
    append(...nodes) { for (const n of nodes) this.appendChild(toNode(n)); },
    prepend(...nodes) { const first = this.firstChild; for (const n of nodes) this.insertBefore(toNode(n), first); },
    replaceChildren(...nodes) { this.textContent = ''; this.append(...nodes); },
  };
  function mixin(target, source) {
    for (const k of Object.getOwnPropertyNames(source))
      Object.defineProperty(target, k, Object.getOwnPropertyDescriptor(source, k));
  }
  function nodeList(arr) {
    arr.item = (i) => arr[i] || null;
    return arr;
  }

  class DocumentFragment extends Node {
    getElementById(id) { return this.querySelector('#' + str(id)); }
  }
  mixin(DocumentFragment.prototype, parentNodeMethods);

  class DOMTokenList {
    constructor(el, attr) { this._el = el; this._attr = attr; }
    _get() { return (this._el.getAttribute(this._attr) || '').split(/\s+/).filter(Boolean); }
    _set(list) { this._el.setAttribute(this._attr, list.join(' ')); }
    get length() { return this._get().length; }
    get value() { return this._get().join(' '); }
    item(i) { return this._get()[i] || null; }
    contains(t) { return this._get().includes(str(t)); }
    add(...ts) { const l = this._get(); for (const t of ts) if (!l.includes(str(t))) l.push(str(t)); this._set(l); }
    remove(...ts) { const r = ts.map(str); this._set(this._get().filter((t) => !r.includes(t))); }
    toggle(t, force) {
      t = str(t);
      const has = this.contains(t);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.add(t);
      if (!want && has) this.remove(t);
      return want;
    }
    replace(a, b) { const l = this._get(); const i = l.indexOf(str(a)); if (i < 0) return false; l[i] = str(b); this._set(l); return true; }
    forEach(fn, self) { this._get().forEach((t, i) => fn.call(self, t, i, this)); }
    toString() { return this.value; }
    [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
  }

  // element.style: reads and writes the style attribute.
  const camelToKebab = (p) => p.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase()).replace(/^(webkit|moz|ms)-/, '-$1-');
  function parseStyle(text) {
    const m = new Map();
    for (const decl of (text || '').split(';')) {
      const i = decl.indexOf(':');
      if (i > 0) m.set(decl.slice(0, i).trim().toLowerCase(), decl.slice(i + 1).trim());
    }
    return m;
  }
  function styleProxy(el) {
    const get = () => parseStyle(el.getAttribute('style'));
    const put = (m) => {
      const text = Array.from(m, ([k, v]) => `${k}: ${v}`).join('; ');
      if (text) el.setAttribute('style', text); else el.removeAttribute('style');
    };
    const api = {
      getPropertyValue: (p) => get().get(str(p).toLowerCase()) || '',
      setProperty: (p, v) => { const m = get(); if (v === null || v === '') m.delete(str(p).toLowerCase()); else m.set(str(p).toLowerCase(), str(v)); put(m); },
      removeProperty: (p) => { const m = get(); const old = m.get(str(p).toLowerCase()) || ''; m.delete(str(p).toLowerCase()); put(m); return old; },
    };
    return new Proxy(api, {
      get(t, p) {
        if (typeof p !== 'string') return undefined;
        if (p in api) return api[p];
        if (p === 'cssText') return el.getAttribute('style') || '';
        if (p === 'length') return get().size;
        return get().get(camelToKebab(p)) || '';
      },
      set(t, p, v) {
        if (typeof p !== 'string') return true;
        if (p === 'cssText') { el.setAttribute('style', str(v)); return true; }
        api.setProperty(camelToKebab(p), v === null ? '' : str(v));
        return true;
      },
    });
  }
  function datasetProxy(el) {
    const attr = (p) => 'data-' + p.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase());
    return new Proxy({}, {
      get: (t, p) => (typeof p === 'string' ? el.getAttribute(attr(p)) ?? undefined : undefined),
      set: (t, p, v) => { if (typeof p === 'string') el.setAttribute(attr(p), str(v)); return true; },
      deleteProperty: (t, p) => { if (typeof p === 'string') el.removeAttribute(attr(p)); return true; },
      has: (t, p) => typeof p === 'string' && el.hasAttribute(attr(p)),
      ownKeys: () => el.getAttributeNames().filter((n) => n.startsWith('data-'))
        .map((n) => n.slice(5).replace(/-([a-z])/g, (m, c) => c.toUpperCase())),
      getOwnPropertyDescriptor: (t, p) => (typeof p === 'string' && el.hasAttribute(attr(p))
        ? { value: el.getAttribute(attr(p)), enumerable: true, configurable: true, writable: true } : undefined),
    });
  }

  const styles = new WeakMap();
  const datasets = new WeakMap();
  const classLists = new WeakMap();

  class Element extends Node {
    get tagName() { return this.nodeName; }
    get localName() { return dom.tag(idOf(this)); }
    get namespaceURI() { return 'http://www.w3.org/1999/xhtml'; }
    get id() { return this.getAttribute('id') || ''; }
    set id(v) { this.setAttribute('id', v); }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() {
      let l = classLists.get(this);
      if (!l) classLists.set(this, (l = new DOMTokenList(this, 'class')));
      return l;
    }
    get style() {
      let s = styles.get(this);
      if (!s) styles.set(this, (s = styleProxy(this)));
      return s;
    }
    set style(v) { this.setAttribute('style', v); }
    get dataset() {
      let d = datasets.get(this);
      if (!d) datasets.set(this, (d = datasetProxy(this)));
      return d;
    }
    getAttribute(n) { return dom.getAttr(idOf(this), str(n)); }
    setAttribute(n, v) { dom.setAttr(idOf(this), str(n), str(v)); }
    removeAttribute(n) { dom.removeAttr(idOf(this), str(n)); }
    hasAttribute(n) { return dom.getAttr(idOf(this), str(n)) !== null; }
    toggleAttribute(n, force) {
      const has = this.hasAttribute(n);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.setAttribute(n, '');
      if (!want && has) this.removeAttribute(n);
      return want;
    }
    getAttributeNames() { return dom.attrNames(idOf(this)); }
    get attributes() {
      const list = this.getAttributeNames().map((name) => ({ name, value: this.getAttribute(name), nodeName: name }));
      list.getNamedItem = (n) => list.find((a) => a.name === str(n).toLowerCase()) || null;
      list.item = (i) => list[i] || null;
      return list;
    }
    hasAttributes() { return this.getAttributeNames().length > 0; }
    get nextElementSibling() { return this._sibling(1, true); }
    get previousElementSibling() { return this._sibling(-1, true); }
    get innerHTML() { return dom.innerHTML(idOf(this)); }
    set innerHTML(v) { dom.setInnerHTML(idOf(this), v === null ? '' : str(v)); }
    get outerHTML() { return dom.outerHTML(idOf(this)); }
    set outerHTML(v) {
      const p = this.parentNode;
      if (!p) return;
      const t = document.createElement('template-holder');
      t.innerHTML = str(v);
      for (const c of t.childNodes) p.insertBefore(c, this);
      this.remove();
    }
    insertAdjacentHTML(pos, html) {
      const t = document.createElement('div');
      t.innerHTML = str(html);
      this._insertAdjacent(str(pos).toLowerCase(), t.childNodes);
    }
    insertAdjacentElement(pos, el) { this._insertAdjacent(str(pos).toLowerCase(), [el]); return el; }
    insertAdjacentText(pos, text) { this._insertAdjacent(str(pos).toLowerCase(), [document.createTextNode(str(text))]); }
    _insertAdjacent(pos, nodes) {
      const p = this.parentNode;
      for (const n of Array.from(nodes)) {
        if (pos === 'beforebegin' && p) p.insertBefore(n, this);
        else if (pos === 'afterbegin') this.insertBefore(n, this.firstChild);
        else if (pos === 'beforeend') this.appendChild(n);
        else if (pos === 'afterend' && p) p.insertBefore(n, this.nextSibling);
      }
    }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    get outerText() { return this.textContent; }
    remove() { dom.remove(idOf(this)); }
    before(...nodes) { const p = this.parentNode; if (p) for (const n of nodes) p.insertBefore(toNode(n), this); }
    after(...nodes) {
      const p = this.parentNode;
      if (!p) return;
      const next = this.nextSibling;
      for (const n of nodes) p.insertBefore(toNode(n), next);
    }
    replaceWith(...nodes) { this.before(...nodes); this.remove(); }
    matches(sel) { return dom.matches(idOf(this), str(sel)); }
    webkitMatchesSelector(sel) { return this.matches(sel); }
    closest(sel) {
      for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(sel)) return n;
      return null;
    }
    getBoundingClientRect() {
      const [x, y, w, h] = dom.rect(idOf(this));
      const top = y - G.scrollY;
      return { x, y: top, left: x, top, width: w, height: h, right: x + w, bottom: top + h, toJSON() { return this; } };
    }
    getClientRects() { return [this.getBoundingClientRect()]; }
    get offsetWidth() { return dom.rect(idOf(this))[2]; }
    get offsetHeight() { return dom.rect(idOf(this))[3]; }
    get offsetLeft() { return dom.rect(idOf(this))[0]; }
    get offsetTop() { return dom.rect(idOf(this))[1]; }
    get offsetParent() { return this.parentElement; }
    get clientWidth() { return this.offsetWidth; }
    get clientHeight() { return this.offsetHeight; }
    get scrollWidth() { return this.offsetWidth; }
    get scrollHeight() { return this.offsetHeight; }
    get scrollTop() { return 0; } set scrollTop(v) {}
    get scrollLeft() { return 0; } set scrollLeft(v) {}
    scrollIntoView() {} scrollTo() {} scroll() {} focus() {} blur() {}
    click() {
      const ev = new MouseEvent('click', { bubbles: true, cancelable: true });
      dispatchOn(this, ev);
    }
    attachShadow() { throw new Error('NotSupportedError: shadow DOM is not supported'); }
  }
  mixin(Element.prototype, parentNodeMethods);
  defineHandlers(Element.prototype);

  // Reflected attributes.
  function reflect(name, attr = name.toLowerCase(), kind = 'string') {
    Object.defineProperty(HTMLElement.prototype, name, {
      get() {
        const v = this.getAttribute(attr);
        if (kind === 'bool') return v !== null;
        if (kind === 'url') return v === null ? '' : dom.resolve(v);
        if (kind === 'number') return v === null ? 0 : Number(v) || 0;
        return v === null ? '' : v;
      },
      set(v) {
        if (kind === 'bool') { if (v) this.setAttribute(attr, ''); else this.removeAttribute(attr); }
        else this.setAttribute(attr, v);
      },
      configurable: true,
    });
  }
  class HTMLElement extends Element {
    get title() { return this.getAttribute('title') || ''; }
    set title(v) { this.setAttribute('title', v); }
    get value() {
      const tag = this.localName;
      if (tag === 'textarea') return this.textContent;
      if (tag === 'select') { const o = this.querySelector('option[selected]') || this.querySelector('option'); return o ? o.value : ''; }
      if (tag === 'option') { const v = this.getAttribute('value'); return v === null ? this.textContent : v; }
      return this.getAttribute('value') || '';
    }
    set value(v) { if (this.localName === 'textarea') this.textContent = v; else this.setAttribute('value', v); }
    get form() { return this.closest('form'); }
    get options() { return this.querySelectorAll('option'); }
    get elements() { return this.querySelectorAll('input, select, textarea, button'); }
    submit() { if (this.localName === 'form') dom.submit(idOf(this), null); }
    requestSubmit(submitter) {
      if (this.localName !== 'form') return;
      const ev = new Event('submit', { bubbles: true, cancelable: true });
      ev.submitter = submitter || null;
      if (dispatchOn(this, ev)) dom.submit(idOf(this), submitter ? idOf(submitter) : null);
    }
    reset() {}
    select() {}
    get content() { // <template>
      const f = document.createDocumentFragment();
      for (const c of this.childNodes) f.appendChild(c.cloneNode(true));
      return f;
    }
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
    get labels() { return []; }
    get validity() { return { valid: true }; }
    checkValidity() { return true; }
    reportValidity() { return true; }
    setCustomValidity() {}
  }
  for (const [n, a, k] of [['hidden', 'hidden', 'bool'], ['href', 'href', 'url'], ['src', 'src', 'url'],
    ['action', 'action', 'url'], ['alt'], ['type'], ['name'], ['rel'], ['target'], ['lang'], ['dir'],
    ['placeholder'], ['htmlFor', 'for'], ['checked', 'checked', 'bool'], ['disabled', 'disabled', 'bool'],
    ['selected', 'selected', 'bool'], ['open', 'open', 'bool'], ['required', 'required', 'bool'],
    ['readOnly', 'readonly', 'bool'], ['multiple', 'multiple', 'bool'], ['async', 'async', 'bool'],
    ['defer', 'defer', 'bool'], ['tabIndex', 'tabindex', 'number'], ['width', 'width', 'number'],
    ['height', 'height', 'number'], ['method'], ['content'], ['media'], ['charset'], ['crossOrigin', 'crossorigin'],
    ['integrity'], ['loading'], ['srcset'], ['sizes'], ['download'], ['colSpan', 'colspan', 'number'],
    ['rowSpan', 'rowspan', 'number'], ['maxLength', 'maxlength', 'number'], ['autocomplete'], ['autofocus', 'autofocus', 'bool']])
    reflect(n, a, k);
  // <a> parts.
  for (const part of ['protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin']) {
    Object.defineProperty(HTMLElement.prototype, part, {
      get() {
        if (!this.hasAttribute('href')) return '';
        try { return new URLImpl(this.href)[part]; } catch (e) { return ''; }
      },
      configurable: true,
    });
  }

  // ---------------------------------------------------------------- document
  class Document extends Node {
    get documentElement() { return this.children[0] || null; }
    get head() { return this.querySelector('head'); }
    get body() { return this.querySelector('body'); }
    set body(v) {}
    get title() { const t = this.querySelector('title'); return t ? t.textContent.trim().replace(/\s+/g, ' ') : ''; }
    set title(v) {
      let t = this.querySelector('title');
      if (!t && this.head) { t = this.createElement('title'); this.head.appendChild(t); }
      if (t) t.textContent = v;
    }
    get readyState() { return readyState; }
    get currentScript() { return currentScript; }
    get location() { return location; }
    set location(v) { location.href = v; }
    get URL() { return dom.url(); }
    get documentURI() { return dom.url(); }
    get baseURI() { return dom.url(); }
    get domain() { return location.hostname; }
    get referrer() { return ''; }
    // The page's non-HttpOnly cookies; the browser decides what's stored.
    get cookie() { return dom.cookie(); }
    set cookie(v) { dom.setCookie(str(v)); }
    get defaultView() { return G; }
    get characterSet() { return 'UTF-8'; }
    get charset() { return 'UTF-8'; }
    get contentType() { return 'text/html'; }
    get compatMode() { return 'CSS1Compat'; }
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    get activeElement() { return this.body; }
    get forms() { return this.querySelectorAll('form'); }
    get images() { return this.querySelectorAll('img'); }
    get links() { return this.querySelectorAll('a[href]'); }
    get scripts() { return this.querySelectorAll('script'); }
    get implementation() { return { hasFeature: () => true }; }
    hasFocus() { return true; }
    getElementById(id) { return wrap(dom.byId(str(id))); }
    getElementsByName(n) { return this.querySelectorAll(`[name]`).filter((e) => e.getAttribute('name') === str(n)); }
    createElement(tag) { return wrap(dom.createElement(str(tag))); }
    createElementNS(ns, tag) { return this.createElement(str(tag).replace(/^.*:/, '')); }
    createTextNode(t) { return wrap(dom.createText(str(t))); }
    createComment() { return wrap(dom.createText('')); }
    createDocumentFragment() { return wrap(dom.createElement('#fragment')); }
    createEvent() { return new Event(''); }
    createRange() {
      return { setStart() {}, setEnd() {}, selectNodeContents() {}, collapse() {},
        createContextualFragment: (html) => { const d = this.createElement('div'); d.innerHTML = str(html); const f = this.createDocumentFragment(); for (const c of d.childNodes) f.appendChild(c); return f; },
        getBoundingClientRect: () => ({ x: 0, y: 0, top: 0, left: 0, width: 0, height: 0, right: 0, bottom: 0 }) };
    }
    importNode(n, deep) { return n.cloneNode(deep); }
    adoptNode(n) { return n; }
    write(...parts) {
      // Without a streaming parser, document.write adds its HTML right
      // after the running script.
      const html = parts.join('');
      const t = this.createElement('div');
      t.innerHTML = html;
      let anchor = currentScript && currentScript.parentNode ? currentScript : null;
      for (const c of t.childNodes) {
        if (anchor) { anchor.after(c); anchor = c; } else if (this.body) this.body.appendChild(c);
      }
    }
    writeln(...parts) { this.write(...parts, '\n'); }
    open() { return this; }
    close() {}
    getSelection() { return { rangeCount: 0, removeAllRanges() {}, addRange() {}, toString: () => '' }; }
    execCommand() { return false; }
    queryCommandSupported() { return false; }
    elementFromPoint() { return null; }
  }
  mixin(Document.prototype, parentNodeMethods);
  defineHandlers(Document.prototype);
  const document = new Document(TOKEN);
  ids.set(document, 0);
  wrappers.set(0, document);
  let readyState = 'loading';

  // ----------------------------------------------------------------- URL
  class URLImpl {
    constructor(href, base) {
      const abs = base === undefined ? str(href) : resolveAgainst(str(base), str(href));
      const m = /^([a-zA-Z][a-zA-Z0-9+.-]*:)(?:\/\/(?:([^@/?#]*)@)?([^/?#:]*)(?::(\d+))?)?([^?#]*)(\?[^#]*)?(#.*)?$/.exec(abs);
      if (!m || (base === undefined && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(abs))) throw new TypeError('Invalid URL: ' + abs);
      this.protocol = m[1].toLowerCase();
      this.username = ''; this.password = '';
      this.hostname = (m[3] || '').toLowerCase();
      this.port = m[4] || '';
      this.pathname = m[5] || (m[3] !== undefined ? '/' : '');
      this.search = m[6] && m[6] !== '?' ? m[6] : '';
      this.hash = m[7] && m[7] !== '#' ? m[7] : '';
    }
    get host() { return this.hostname + (this.port ? ':' + this.port : ''); }
    get origin() { return this.hostname ? `${this.protocol}//${this.host}` : 'null'; }
    get href() { return `${this.protocol}${this.hostname ? '//' + this.host : ''}${this.pathname}${this.search}${this.hash}`; }
    get searchParams() { return new URLSearchParams(this.search); }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }
  function resolveAgainst(base, href) {
    // The browser's resolver only knows the page URL; good enough for
    // relative paths against other absolute bases too.
    if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href)) return href;
    const b = new URLImpl(base);
    if (href.startsWith('//')) return b.protocol + href;
    if (href.startsWith('/')) return b.origin + href;
    if (href.startsWith('?')) return b.origin + b.pathname + href;
    if (href.startsWith('#')) return b.origin + b.pathname + b.search + href;
    const dir = b.pathname.replace(/[^/]*$/, '');
    const parts = [];
    for (const seg of (dir + href).split('/')) {
      if (seg === '..') parts.pop(); else if (seg !== '.') parts.push(seg);
    }
    return b.origin + parts.join('/').replace(/^(?!\/)/, '/');
  }
  class URLSearchParams {
    constructor(init = '') {
      this._p = [];
      if (typeof init === 'string') {
        for (const part of init.replace(/^\?/, '').split('&')) {
          if (!part) continue;
          const [k, v = ''] = part.split('=');
          const d = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch (e) { return s; } };
          this._p.push([d(k), d(v)]);
        }
      } else if (init && typeof init === 'object') {
        for (const [k, v] of (Symbol.iterator in init ? init : Object.entries(init))) this._p.push([str(k), str(v)]);
      }
    }
    get(k) { const e = this._p.find((p) => p[0] === str(k)); return e ? e[1] : null; }
    getAll(k) { return this._p.filter((p) => p[0] === str(k)).map((p) => p[1]); }
    has(k) { return this._p.some((p) => p[0] === str(k)); }
    set(k, v) { this.delete(k); this._p.push([str(k), str(v)]); }
    append(k, v) { this._p.push([str(k), str(v)]); }
    delete(k) { this._p = this._p.filter((p) => p[0] !== str(k)); }
    forEach(fn) { this._p.forEach(([k, v]) => fn(v, k, this)); }
    keys() { return this._p.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._p.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this._p.map((p) => p.slice())[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return this._p.map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&'); }
  }

  // ---------------------------------------------------------------- location
  const location = {
    get href() { return dom.url(); },
    set href(v) { dom.navigate(str(v)); },
    get protocol() { return new URLImpl(dom.url()).protocol; },
    get host() { return new URLImpl(dom.url()).host; },
    get hostname() { return new URLImpl(dom.url()).hostname; },
    get port() { return new URLImpl(dom.url()).port; },
    get pathname() { return new URLImpl(dom.url()).pathname; },
    get search() { return new URLImpl(dom.url()).search; },
    set search(v) { const u = new URLImpl(dom.url()); dom.navigate(u.origin + u.pathname + (str(v).startsWith('?') || !v ? str(v) : '?' + v)); },
    get hash() { return new URLImpl(dom.url()).hash; },
    set hash(v) { const u = new URLImpl(dom.url()); dom.setUrl(u.origin + u.pathname + u.search + (str(v).startsWith('#') ? str(v) : '#' + v)); },
    get origin() { return new URLImpl(dom.url()).origin; },
    assign(u) { dom.navigate(str(u)); },
    replace(u) { dom.navigate(str(u)); },
    reload() { dom.navigate(dom.url()); },
    toString() { return dom.url(); },
  };
  const history = {
    get length() { return 1; },
    state: null,
    pushState(state, title, url) { this.state = state; if (url !== undefined && url !== null) dom.setUrl(str(url)); },
    replaceState(state, title, url) { this.state = state; if (url !== undefined && url !== null) dom.setUrl(str(url)); },
    back() {}, forward() {}, go() {},
    scrollRestoration: 'auto',
  };

  // ------------------------------------------------------------------ timers
  // A virtual clock: callbacks run in due order, in bounded batches driven
  // by the renderer, without real waiting.
  let clock = 0;
  let timerSeq = 0;
  const timers = new Map(); // id -> {due, fn, args, every}
  function addTimer(fn, ms, args, every) {
    if (typeof fn !== 'function') {
      const code = str(fn);
      fn = () => (0, eval)(code);
    }
    const id = ++timerSeq;
    const delay = Math.max(0, Number(ms) || 0);
    timers.set(id, { due: clock + delay, fn, args, every: every ? Math.max(delay, 4) : 0, seq: id });
    return id;
  }
  function stepTimers(windowMs) {
    // Runs the next due timer within the window; false when none is left.
    let next = null;
    for (const [id, t] of timers) {
      if (!next || t.due < next[1].due || (t.due === next[1].due && t.seq < next[1].seq)) next = [id, t];
    }
    if (!next || next[1].due > windowEnd) return false;
    const [id, t] = next;
    clock = Math.max(clock, t.due);
    if (t.every) { t.due = clock + t.every; t.seq = ++timerSeq; } else timers.delete(id);
    try { t.fn.apply(G, t.args); } catch (e) { reportError(e); }
    return true;
  }
  let windowEnd = 0;
  function startWindow(ms) { windowEnd = clock + ms; }

  // ------------------------------------------------------------- storage etc
  class Storage {
    constructor() { Object.defineProperty(this, '_m', { value: new Map() }); }
    get length() { return this._m.size; }
    key(i) { return Array.from(this._m.keys())[i] ?? null; }
    getItem(k) { return this._m.has(str(k)) ? this._m.get(str(k)) : null; }
    setItem(k, v) { this._m.set(str(k), str(v)); }
    removeItem(k) { this._m.delete(str(k)); }
    clear() { this._m.clear(); }
  }

  const navigator = {
    userAgent: dom.ua,
    appVersion: '5.0 (X11)',
    appName: 'Netscape',
    appCodeName: 'Mozilla',
    product: 'Gecko',
    productSub: '20100101',
    vendor: '',
    vendorSub: '',
    platform: 'Linux x86_64',
    oscpu: 'Linux x86_64',
    language: 'en-US',
    languages: Object.freeze(['en-US', 'en']),
    onLine: true,
    cookieEnabled: true,
    doNotTrack: 'unspecified',
    webdriver: false,
    hardwareConcurrency: 4,
    maxTouchPoints: 0,
    pdfViewerEnabled: true,
    plugins: Object.freeze([]),
    mimeTypes: Object.freeze([]),
    javaEnabled: () => false,
    sendBeacon: () => false, // a tracking API: never sends
    clipboard: undefined,
    serviceWorker: undefined,
  };
  Object.freeze(navigator);

  const performance = {
    now: () => clock,
    timeOrigin: Date.now(),
    mark() {}, measure() {}, clearMarks() {}, clearMeasures() {},
    getEntries: () => [], getEntriesByName: () => [], getEntriesByType: () => [],
  };

  // ---------------------------------------------------------------- network
  // Scripts' requests go to the browser, which applies every rule (your
  // rules, the blocker, HTTPS-only, CORS, cookies) and answers later.
  const pending = new Map(); // request id -> {resolve, reject}

  class Headers {
    constructor(init) {
      this._h = [];
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init && typeof init === 'object') for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    append(k, v) { this._h.push([str(k).toLowerCase(), str(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) {
      const vals = this._h.filter((h) => h[0] === str(k).toLowerCase()).map((h) => h[1]);
      return vals.length ? vals.join(', ') : null;
    }
    has(k) { return this._h.some((h) => h[0] === str(k).toLowerCase()); }
    delete(k) { this._h = this._h.filter((h) => h[0] !== str(k).toLowerCase()); }
    forEach(fn, self) { for (const [k, v] of this.entries()) fn.call(self, v, k, this); }
    *entries() {
      const names = [...new Set(this._h.map((h) => h[0]))].sort();
      for (const n of names) yield [n, this.get(n)];
    }
    *keys() { for (const [k] of this.entries()) yield k; }
    *values() { for (const [, v] of this.entries()) yield v; }
    [Symbol.iterator]() { return this.entries(); }
  }

  const utf8 = (s) => {
    const out = [];
    for (const ch of str(s)) {
      let c = ch.codePointAt(0);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    }
    return new Uint8Array(out);
  };
  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s = '') { return utf8(s); }
  }
  class TextDecoder {
    get encoding() { return 'utf-8'; }
    decode(buf) {
      if (!buf) return '';
      const b = buf instanceof ArrayBuffer ? new Uint8Array(buf) : new Uint8Array(buf.buffer || buf, buf.byteOffset || 0, buf.byteLength);
      let s = '';
      for (let i = 0; i < b.length;) {
        const c = b[i++];
        let cp;
        if (c < 0x80) cp = c;
        else if (c < 0xe0) cp = ((c & 31) << 6) | (b[i++] & 63);
        else if (c < 0xf0) cp = ((c & 15) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
        else cp = ((c & 7) << 18) | ((b[i++] & 63) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
        s += String.fromCodePoint(cp > 0x10ffff ? 0xfffd : cp);
      }
      return s;
    }
  }

  class FormData {
    constructor(form) {
      this._e = [];
      if (form && form.querySelectorAll) {
        for (const el of form.querySelectorAll('input, select, textarea')) {
          const name = el.getAttribute('name');
          if (!name || el.hasAttribute('disabled')) continue;
          const type = (el.getAttribute('type') || '').toLowerCase();
          if ((type === 'checkbox' || type === 'radio') && !el.hasAttribute('checked')) continue;
          if (['submit', 'button', 'reset', 'image', 'file'].includes(type)) continue;
          this.append(name, el.value);
        }
      }
    }
    append(k, v) { this._e.push([str(k), str(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { const e = this._e.find((x) => x[0] === str(k)); return e ? e[1] : null; }
    getAll(k) { return this._e.filter((x) => x[0] === str(k)).map((x) => x[1]); }
    has(k) { return this._e.some((x) => x[0] === str(k)); }
    delete(k) { this._e = this._e.filter((x) => x[0] !== str(k)); }
    forEach(fn, self) { for (const [k, v] of this._e) fn.call(self, v, k, this); }
    *entries() { yield* this._e.map((e) => e.slice()); }
    *keys() { for (const [k] of this._e) yield k; }
    *values() { for (const [, v] of this._e) yield v; }
    [Symbol.iterator]() { return this.entries(); }
  }

  // A request body as text, and the content type it implies.
  function encodeBody(body) {
    if (body === undefined || body === null) return [null, null];
    if (body instanceof URLSearchParams) return [body.toString(), 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (body instanceof FormData) {
      const boundary = '----brickle' + Math.floor(Math.random() * 1e16).toString(36);
      let out = '';
      for (const [k, v] of body._e) {
        const q = (x) => x.replace(/"/g, '%22').replace(/\r|\n/g, ' ');
        out += `--${boundary}\r\nContent-Disposition: form-data; name="${q(k)}"\r\n\r\n${v}\r\n`;
      }
      return [out + `--${boundary}--\r\n`, 'multipart/form-data; boundary=' + boundary];
    }
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) return [new TextDecoder().decode(body), null];
    if (typeof body === 'object' && typeof body.text === 'function' && body._text !== undefined) return [body._text, body.type || null];
    return [str(body), 'text/plain;charset=UTF-8'];
  }

  class AbortSignal extends EventTarget {
    constructor(token) { super(); if (token !== TOKEN) illegal(); this.aborted = false; this.reason = undefined; this.onabort = null; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason) { const c = new AbortController(); c.abort(reason); return c.signal; }
    static timeout(ms) {
      const c = new AbortController();
      addTimer(() => c.abort(new Error('TimeoutError: signal timed out')), ms, [], false);
      return c.signal;
    }
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal(TOKEN); }
    abort(reason) {
      const s = this.signal;
      if (s.aborted) return;
      s.aborted = true;
      s.reason = reason === undefined ? new Error('AbortError: the operation was aborted') : reason;
      const ev = new Event('abort');
      if (typeof s.onabort === 'function') try { s.onabort(ev); } catch (e) { reportError(e); }
      dispatchOn(s, ev);
    }
  }

  const STATUS_TEXT = { 200: 'OK', 201: 'Created', 204: 'No Content', 301: 'Moved Permanently', 302: 'Found',
    304: 'Not Modified', 400: 'Bad Request', 401: 'Unauthorized', 403: 'Forbidden', 404: 'Not Found',
    405: 'Method Not Allowed', 409: 'Conflict', 429: 'Too Many Requests', 500: 'Internal Server Error',
    502: 'Bad Gateway', 503: 'Service Unavailable' };

  class Response {
    constructor(body = null, init = {}) {
      this._text = body === null || body === undefined ? '' : str(body);
      this.status = init.status === undefined ? 200 : init.status | 0;
      this.statusText = init.statusText !== undefined ? str(init.statusText) : (STATUS_TEXT[this.status] || '');
      this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      this.url = init._url || '';
      this.redirected = !!init._redirected;
      this.type = init._type || 'default';
      this.bodyUsed = false;
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    _consume() {
      if (this.bodyUsed) return Promise.reject(new TypeError('Body has already been consumed'));
      this.bodyUsed = true;
      return Promise.resolve(this._text);
    }
    text() { return this._consume(); }
    json() { return this._consume().then((t) => JSON.parse(t)); }
    arrayBuffer() { return this._consume().then((t) => utf8(t).buffer); }
    blob() { return this._consume().then((t) => ({ size: utf8(t).length, type: this.headers.get('content-type') || '', text: () => Promise.resolve(t), _text: t })); }
    formData() { return this._consume().then((t) => { const f = new FormData(); new URLSearchParams(t).forEach((v, k) => f.append(k, v)); return f; }); }
    clone() {
      if (this.bodyUsed) throw new TypeError('Body has already been consumed');
      return new Response(this._text, { status: this.status, statusText: this.statusText, headers: new Headers(this.headers),
        _url: this.url, _redirected: this.redirected, _type: this.type });
    }
    static json(data, init = {}) {
      const h = new Headers(init.headers);
      if (!h.has('content-type')) h.set('content-type', 'application/json');
      return new Response(JSON.stringify(data), { ...init, headers: h });
    }
    static error() { const r = new Response(null, { status: 0 }); r.type = 'error'; return r; }
  }

  class Request {
    constructor(input, init = {}) {
      const base = input instanceof Request ? input : null;
      this.url = dom.resolve(base ? base.url : str(input));
      this.method = str(init.method || (base && base.method) || 'GET').toUpperCase();
      this.headers = new Headers(init.headers || (base && base.headers));
      this.credentials = init.credentials || (base && base.credentials) || 'same-origin';
      this.signal = init.signal || (base && base.signal) || null;
      this.mode = init.mode || 'cors';
      this._body = init.body !== undefined ? init.body : base ? base._body : null;
    }
    clone() { return new Request(this); }
  }

  // Send a request to the browser; `done(err, response)` is called later.
  function send(method, url, headers, body, credentials, done) {
    const [text, implied] = encodeBody(body);
    const h = headers instanceof Headers ? headers : new Headers(headers);
    if (implied && !h.has('content-type')) h.set('content-type', implied);
    const flat = [];
    for (const [k, v] of h) flat.push(k, v);
    const id = dom.request(method, url, flat, text, credentials);
    pending.set(id, done);
    return id;
  }

  function fetch(input, init = {}) {
    let req;
    try { req = new Request(input, init); } catch (e) { return Promise.reject(e); }
    if (req.signal && req.signal.aborted) return Promise.reject(req.signal.reason);
    return new Promise((resolve, reject) => {
      let settled = false;
      if (req.signal) req.signal.addEventListener('abort', () => { if (!settled) { settled = true; reject(req.signal.reason); } });
      try {
        send(req.method, req.url, req.headers, req._body, req.credentials, (err, r) => {
          if (settled) return;
          settled = true;
          if (err !== null) { reject(new TypeError('NetworkError when attempting to fetch resource: ' + err)); return; }
          resolve(new Response(r.method === 'HEAD' ? '' : r.body, { status: r.status, headers: new Headers(r.headers),
            _url: r.url, _redirected: r.redirected, _type: new URLImpl(r.url).origin === location.origin ? 'basic' : 'cors' }));
        });
      } catch (e) { settled = true; reject(new TypeError('NetworkError: ' + (e && e.message))); }
    });
  }

  class XMLHttpRequest extends EventTarget {
    constructor() {
      super();
      this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = '';
      this.responseType = ''; this.responseURL = ''; this.withCredentials = false; this.timeout = 0;
      this._headers = new Headers(); this._respHeaders = new Headers(); this._aborted = false;
      this.upload = new EventTarget();
    }
    open(method, url, async = true) {
      if (async === false) dom.log('warn', '[brickle] synchronous XMLHttpRequest runs asynchronously in Brickle');
      this._method = str(method).toUpperCase();
      this._url = dom.resolve(str(url));
      this._headers = new Headers();
      this._aborted = false;
      this._state(1);
    }
    setRequestHeader(k, v) { this._headers.append(k, v); }
    getResponseHeader(k) { return this._respHeaders.get(k); }
    getAllResponseHeaders() { let s = ''; for (const [k, v] of this._respHeaders) s += `${k}: ${v}\r\n`; return s; }
    overrideMimeType() {}
    abort() { this._aborted = true; if (this.readyState > 0 && this.readyState < 4) { this._state(4); this._fire('abort'); this._fire('loadend'); } this.readyState = 0; }
    _fire(type) {
      const ev = new Event(type);
      const h = this['on' + type];
      if (typeof h === 'function') try { h.call(this, ev); } catch (e) { reportError(e); }
      dispatchOn(this, ev);
    }
    _state(n) { this.readyState = n; this._fire('readystatechange'); }
    send(body = null) {
      if (this.readyState !== 1) throw new Error('InvalidStateError: open() first');
      this._fire('loadstart');
      send(this._method, this._url, this._headers, body, this.withCredentials ? 'include' : 'same-origin', (err, r) => {
        if (this._aborted) return;
        if (err !== null) {
          this.status = 0;
          this._state(4);
          this._fire('error');
          this._fire('loadend');
          return;
        }
        this.status = r.status;
        this.statusText = STATUS_TEXT[r.status] || '';
        this.responseURL = r.url;
        this._respHeaders = new Headers(r.headers);
        this._state(2);
        this._state(3);
        this.responseText = r.body;
        if (this.responseType === 'json') { try { this.response = JSON.parse(r.body); } catch (e) { this.response = null; } }
        else if (this.responseType === 'arraybuffer') this.response = utf8(r.body).buffer;
        else this.response = r.body;
        this._state(4);
        this._fire('load');
        this._fire('loadend');
      });
    }
  }
  XMLHttpRequest.UNSENT = 0; XMLHttpRequest.OPENED = 1; XMLHttpRequest.HEADERS_RECEIVED = 2;
  XMLHttpRequest.LOADING = 3; XMLHttpRequest.DONE = 4;
  for (const n of ['onreadystatechange', 'onload', 'onerror', 'onloadend', 'onloadstart', 'onabort', 'ontimeout', 'onprogress'])
    XMLHttpRequest.prototype[n] = null;

  // Called by the renderer with the browser's answer.
  function deliver(id, err, status, url, redirected, headers, body) {
    const done = pending.get(id);
    if (!done) return;
    pending.delete(id);
    if (err !== null && err !== undefined) { done(str(err), null); return; }
    const pairs = [];
    for (let i = 0; i + 1 < headers.length; i += 2) pairs.push([headers[i], headers[i + 1]]);
    done(null, { status, url, redirected, headers: pairs, body });
  }

  class Observer {
    constructor(cb) { this._cb = cb; }
    observe() {} unobserve() {} disconnect() {} takeRecords() { return []; }
  }
  class IntersectionObserver extends Observer {
    // Everything counts as visible (Brickle lays out the whole page).
    observe(target) {
      addTimer(() => this._cb([{ target, isIntersecting: true, intersectionRatio: 1,
        boundingClientRect: target.getBoundingClientRect(), time: clock }], this), 0, [], false);
    }
  }

  const screen = Object.freeze({
    width: 1920, height: 1080, availWidth: 1920, availHeight: 1080, colorDepth: 24, pixelDepth: 24,
    orientation: { type: 'landscape-primary', angle: 0, addEventListener() {} },
  });

  function matchMedia(q) {
    q = str(q);
    const test = (expr) => {
      let m;
      if ((m = /min-width:\s*([\d.]+)px/.exec(expr))) return viewport[0] >= +m[1];
      if ((m = /max-width:\s*([\d.]+)px/.exec(expr))) return viewport[0] <= +m[1];
      if (/prefers-color-scheme:\s*light/.test(expr)) return true;
      if (/prefers-reduced-motion:\s*no-preference/.test(expr)) return true;
      if (/^\s*(all|screen)\s*$/.test(expr)) return true;
      return false;
    };
    const matches = q.split(',').some((part) => part.split(/\band\b/).every(test));
    return { matches, media: q, onchange: null, addListener() {}, removeListener() {},
      addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true };
  }

  function getComputedStyle(el) {
    // Only inline styles are visible to scripts so far.
    const s = el && el.style ? el.style : styleProxy({ getAttribute: () => '', setAttribute() {}, removeAttribute() {} });
    return new Proxy(s, { get: (t, p) => (p === 'display' && el && el.hidden ? 'none' : t[p]) });
  }

  const consoleObj = {};
  for (const level of ['log', 'info', 'warn', 'error', 'debug', 'trace']) {
    consoleObj[level] = (...args) => dom.log(level, args.map((a) => {
      if (typeof a === 'string') return a;
      if (a instanceof Error) return a.toString();
      try { return typeof a === 'object' && a !== null ? JSON.stringify(a) : String(a); } catch (e) { return String(a); }
    }).join(' '));
  }
  for (const k of ['group', 'groupCollapsed', 'groupEnd', 'time', 'timeEnd', 'timeLog', 'count', 'assert', 'table', 'dir', 'clear'])
    consoleObj[k] = () => {};

  // Base64 of "binary strings" (one byte per character).
  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const badChars = (what) => Object.assign(new Error(`${what}: invalid character`), { name: 'InvalidCharacterError' });
  function btoa(data) {
    const s = str(data);
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
      const c = [s.charCodeAt(i), s.charCodeAt(i + 1), s.charCodeAt(i + 2)];
      if (c.some((x) => x > 255)) throw badChars('btoa');
      const n = (c[0] << 16) | ((c[1] || 0) << 8) | (c[2] || 0);
      out += B64[n >> 18] + B64[(n >> 12) & 63];
      out += i + 1 < s.length ? B64[(n >> 6) & 63] : '=';
      out += i + 2 < s.length ? B64[n & 63] : '=';
    }
    return out;
  }
  function atob(data) {
    let s = str(data).replace(/[\t\n\f\r ]/g, '');
    if (s.length % 4 === 0) s = s.replace(/==?$/, '');
    if (s.length % 4 === 1 || /[^A-Za-z0-9+/]/.test(s)) throw badChars('atob');
    let out = '', bits = 0, n = 0;
    for (const ch of s) {
      n = (n << 6) | B64.indexOf(ch);
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((n >> bits) & 255);
      }
    }
    return out;
  }

  // ------------------------------------------------------------ the globals
  const defs = {
    window: G, self: G, top: G, parent: G, frames: G, globalThis: G,
    document, location, history, navigator, screen, performance, console: consoleObj,
    Node, Element, HTMLElement, Text, Comment, CharacterData, Document, HTMLDocument: Document, DocumentFragment,
    EventTarget, Event, CustomEvent, UIEvent, MouseEvent, KeyboardEvent, FocusEvent, DOMTokenList,
    URL: URLImpl, URLSearchParams, XMLHttpRequest, Storage,
    localStorage: new Storage(), sessionStorage: new Storage(),
    MutationObserver: Observer, ResizeObserver: Observer, IntersectionObserver, PerformanceObserver: Observer,
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, args, false),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, args, true),
    clearTimeout: (id) => { timers.delete(id); },
    clearInterval: (id) => { timers.delete(id); },
    requestAnimationFrame: (fn) => addTimer(() => fn(clock), 16, [], false),
    cancelAnimationFrame: (id) => { timers.delete(id); },
    requestIdleCallback: (fn) => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1, [], false),
    cancelIdleCallback: (id) => { timers.delete(id); },
    queueMicrotask: (fn) => { Promise.resolve().then(fn); },
    fetch, Headers, Request, Response, AbortController, AbortSignal, FormData, TextEncoder, TextDecoder,
    btoa, atob,
    // Plain elements: setting src requests nothing until the image is put
    // on the page (a hidden `new Image()` is the usual tracking pixel).
    Image: function Image(w, h) {
      const img = document.createElement('img');
      if (w !== undefined) img.setAttribute('width', w);
      if (h !== undefined) img.setAttribute('height', h);
      return img;
    },
    Option: function Option(text, value, defSel, sel) {
      const o = document.createElement('option');
      if (text !== undefined) o.textContent = text;
      if (value !== undefined) o.setAttribute('value', value);
      if (sel) o.setAttribute('selected', '');
      return o;
    },
    matchMedia, getComputedStyle,
    alert: (m) => dom.log('alert', str(m)), confirm: () => false, prompt: () => null, print() {},
    open: () => null, close() {}, focus() {}, blur() {}, stop() {},
    scrollTo() {}, scrollBy() {}, scroll() {},
    getSelection: () => document.getSelection(),
    isSecureContext: true, origin: undefined, name: '', closed: false, opener: null, frameElement: null,
    devicePixelRatio: 1, scrollX: 0, scrollY: 0, pageXOffset: 0, pageYOffset: 0,
    reportError,
  };
  for (const [k, v] of Object.entries(defs)) {
    Object.defineProperty(G, k, { value: v, writable: true, configurable: true, enumerable: false });
  }
  Object.defineProperty(G, 'origin', { get: () => location.origin, configurable: true });
  for (const [k, i] of [['innerWidth', 0], ['outerWidth', 0], ['innerHeight', 1], ['outerHeight', 1]])
    Object.defineProperty(G, k, { get: () => viewport[i], configurable: true });
  Object.assign(G, eventTargetMethods);
  defineHandlers(G);
  // Names Firefox has that some scripts probe for; Chrome-only ones are absent.
  for (const k of ['HTMLDivElement', 'HTMLAnchorElement', 'HTMLInputElement', 'HTMLFormElement', 'HTMLImageElement',
    'HTMLScriptElement', 'HTMLButtonElement', 'HTMLSelectElement', 'HTMLTextAreaElement', 'HTMLIFrameElement',
    'HTMLCanvasElement', 'HTMLVideoElement', 'HTMLStyleElement', 'HTMLLinkElement', 'HTMLTemplateElement',
    'HTMLBodyElement', 'HTMLSpanElement', 'HTMLParagraphElement', 'HTMLUListElement', 'HTMLLIElement', 'SVGElement'])
    Object.defineProperty(G, k, { value: HTMLElement, writable: true, configurable: true });

  // -------------------------------------------------- hooks for the renderer
  function fire(target, type) {
    dispatchOn(target, new Event(type, { bubbles: false, cancelable: false }));
  }
  return {
    // Set document.currentScript around each script (id, or -1 for none).
    runScript(_code, id) { currentScript = id === -1 ? null : wrap(id); },
    // A batch starts with a window (ms of virtual time); later calls in the
    // same batch pass -1.
    stepTimers(windowMs) {
      if (windowMs >= 0) startWindow(windowMs);
      return stepTimers();
    },
    dispatch(id, type) {
      const target = wrap(id);
      const ev = new MouseEvent(type, { bubbles: true, cancelable: true, button: 0 });
      ev.isTrusted = true;
      dispatchOn(target, ev);
      return ev.defaultPrevented;
    },
    fireEvent(id, type, bubbles, cancelable) {
      const ev = new Event(type, { bubbles, cancelable });
      ev.isTrusted = true;
      dispatchOn(wrap(id), ev);
      return ev.defaultPrevented;
    },
    fireLoad() {
      readyState = 'interactive';
      fire(document, 'readystatechange');
      dispatchOn(document, new Event('DOMContentLoaded', { bubbles: true }));
      readyState = 'complete';
      fire(document, 'readystatechange');
      fire(G, 'load');
      fire(G, 'pageshow');
    },
    setViewport(w, h) { viewport = [w, h]; },
    deliver,
  };
});
