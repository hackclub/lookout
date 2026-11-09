// GPUI owns Wayland keyboard focus and IME. Mirror editable selection state
// and apply committed/composing text through WebKit's editing commands.
(() => {
  let revision = 0;
  const editable = () => {
    const e = document.activeElement;
    return e && ((e instanceof HTMLInputElement && /^(text|search|url|tel|email|password)$/.test(e.type)) ||
      e instanceof HTMLTextAreaElement || e.isContentEditable) ? e : null;
  };
  const state = () => {
    const e = editable();
    if (!e) return window.parent.postMessage({ type: 'lookout:input-state', revision, text: null });
    let text, start, end;
    if ('value' in e) {
      text = e.value; start = e.selectionStart || 0; end = e.selectionEnd || start;
    } else {
      text = e.textContent || '';
      const s = window.getSelection();
      const offset = (node, n) => {
        if (!node || !e.contains(node)) return 0;
        const r = document.createRange(); r.selectNodeContents(e); r.setEnd(node, n);
        return r.toString().length;
      };
      const a = offset(s?.anchorNode, s?.anchorOffset || 0), b = offset(s?.focusNode, s?.focusOffset || 0);
      start = Math.min(a,b); end = Math.max(a,b);
    }
    const r = e.getBoundingClientRect();
    window.parent.postMessage({ type: 'lookout:input-state', revision, text, start, end,
      rect: [r.x, r.y, r.width, r.height] });
  };
  const select = (e, start, end) => {
    if ('value' in e) { try { e.setSelectionRange(start,end); } catch (_) {} return; }
    const walk = document.createTreeWalker(e, NodeFilter.SHOW_TEXT);
    let node, offset = 0, first = null, last = null;
    while ((node = walk.nextNode())) {
      const next = offset + node.length;
      if (!first && start <= next) first = [node, Math.max(0,start-offset)];
      if (end <= next) { last = [node, Math.max(0,end-offset)]; break; }
      offset = next;
    }
    if (!first) return;
    last ||= first;
    const r = document.createRange(); r.setStart(...first); r.setEnd(...last);
    const s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
  };
  Object.defineProperty(window, '__lookoutEdit', { value: (op, range, text, selection, serial) => {
    revision = serial;
    const e = editable(); if (!e) return;
    if (range) select(e, range[0],range[1]);
    if (op === 'replace') document.execCommand('insertText',false,text);
    if (selection) select(e,selection[0],selection[1]);
    state();
  }});
  Object.defineProperty(window, '__lookoutScroll', { value: ({x,y,dx,dy,...modifiers}) => {
    const target = document.elementFromPoint(x,y);
    if (!target) return;
    const event = new WheelEvent('wheel', { clientX:x, clientY:y, deltaX:dx, deltaY:dy,
      deltaMode:WheelEvent.DOM_DELTA_PIXEL, bubbles:true, cancelable:true, ...modifiers });
    if (!target.dispatchEvent(event)) return;
    if (modifiers.shiftKey && !dx) { dx = dy; dy = 0; }
    // Preserve nested scrolling and scroll chaining, including a child's
    // overscroll-behavior. Synthetic WheelEvents have no browser default action.
    const root = document.scrollingElement;
    for (let e = target; e; e = e.parentElement) {
      const style = getComputedStyle(e), isRoot = e === root;
      const oldX = e.scrollLeft, oldY = e.scrollTop;
      const canX = isRoot || /^(auto|scroll|overlay)$/.test(style.overflowX);
      const canY = isRoot || /^(auto|scroll|overlay)$/.test(style.overflowY);
      if (canX || canY) e.scrollBy({left:canX ? dx : 0, top:canY ? dy : 0, behavior:'instant'});
      if (canX) {
        dx -= e.scrollLeft - oldX;
        if (/^(contain|none)$/.test(style.overscrollBehaviorX)) dx = 0;
      }
      if (canY) {
        dy -= e.scrollTop - oldY;
        if (/^(contain|none)$/.test(style.overscrollBehaviorY)) dy = 0;
      }
      if ((!dx && !dy) || isRoot) break;
    }
    state();
  }});
  document.addEventListener('scroll',state,true);
  for (const event of ['focusin','focusout','input','selectionchange','keyup','mouseup'])
    document.addEventListener(event,state,true);
  document.addEventListener('DOMContentLoaded',state);
})();
