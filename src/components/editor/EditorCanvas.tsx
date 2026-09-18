import { useCallback, useEffect, useRef, useState } from "react";

import { editorSession, useEditor } from "../../state/editorStore";
import type { Box, EditorCommand, NodeSpec, SnapGuide } from "../../wasm/abi";
import {
  commandAffine,
  boxAfterCommand,
  fitView,
  gestureFor,
  HANDLE_RADIUS,
  handlePositions,
  hitHandle,
  isDrag,
  marqueeBox,
  rotateCommandFor,
  scaleCommandFor,
  toDocPoint,
  toScreenPoint,
  type HandleId,
  type View,
} from "./canvasModel";

/** A gesture in progress: a marquee, a move, or a transform handle drag. */
type Drag =
  | { kind: "marquee"; from: { x: number; y: number }; to: { x: number; y: number } }
  | {
      kind: "move";
      from: { x: number; y: number };
      command: EditorCommand | null;
      moved: boolean;
    }
  | { kind: "scale"; handle: HandleId; box: Box; command: EditorCommand | null }
  | { kind: "rotate"; box: Box; from: { x: number; y: number }; command: EditorCommand | null };

/** Margin around the document when it is fitted into the canvas. */
const MARGIN = 12;

/**
 * The editing surface.
 *
 * It draws straight from the session's node list and turns every pointer gesture
 * into exactly one command when the gesture *ends*: dragging across the canvas
 * never round-trips to Rust, and one drag is one undo step rather than one per
 * frame (ARCHITECTURE §2). A gesture in progress is drawn from the geometry the
 * canvas already holds plus the gesture's own affine — painting costs nothing —
 * while the same command is handed to the engine as a *preview*, so the module's
 * node records, bounds and snap answers agree with what is on screen and the
 * commit at the end of the gesture is provably the same spec.
 */
export function EditorCanvas() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const nodes = useEditor((s) => s.nodes);
  const selection = useEditor((s) => s.selection);
  const selectionBox = useEditor((s) => s.selectionBox);
  const guides = useEditor((s) => s.guides);
  const revision = useEditor((s) => s.revision);
  const width = useEditor((s) => s.width);
  const height = useEditor((s) => s.height);
  const click = useEditor((s) => s.click);
  const marquee = useEditor((s) => s.marquee);
  const snapMove = useEditor((s) => s.snapMove);
  const preview = useEditor((s) => s.preview);
  const commit = useEditor((s) => s.commit);
  const setGuides = useEditor((s) => s.setGuides);
  const [drag, setDrag] = useState<Drag | null>(null);

  /** Document units per canvas pixel, plus the centring offset. */
  const view = useCallback((): View => {
    const canvas = canvasRef.current;
    if (!canvas) return { scale: 1, offsetX: 0, offsetY: 0 };
    return fitView(canvas.clientWidth, canvas.clientHeight, width, height, MARGIN);
  }, [height, width]);

  const toDoc = useCallback(
    (clientX: number, clientY: number): { x: number; y: number } => {
      const canvas = canvasRef.current;
      if (!canvas) return { x: 0, y: 0 };
      const rect = canvas.getBoundingClientRect();
      return toDocPoint(clientX - rect.left, clientY - rect.top, view());
    },
    [view],
  );

  const toScreen = useCallback(
    (clientX: number, clientY: number): { x: number; y: number } => {
      const canvas = canvasRef.current;
      if (!canvas) return { x: 0, y: 0 };
      const rect = canvas.getBoundingClientRect();
      return { x: clientX - rect.left, y: clientY - rect.top };
    },
    [],
  );

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    const context = canvas?.getContext("2d");
    if (!canvas || !context) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
    canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
    const { scale, offsetX, offsetY } = view();
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, canvas.clientWidth, canvas.clientHeight);

    // The live transform of the gesture in progress, applied in document space
    // (so `context.transform(cmd)` comes before the node's own matrix).
    const live =
      drag && drag.kind !== "marquee" && drag.command ? commandAffine(drag.command) : null;

    context.save();
    context.translate(offsetX, offsetY);
    context.scale(scale, scale);
    context.fillStyle = "#14161a";
    context.fillRect(0, 0, width, height);

    const selected = new Set(selection);
    for (const node of nodes) {
      if (!node.visible) continue;
      const [r, g, b, a] = node.fill;
      context.save();
      if (live && selected.has(node.id)) context.transform(...live);
      // The node's own affine, in canvas argument order.
      const [m0, m1, m2, m3, m4, m5] = node.m;
      context.transform(m0, m1, m2, m3, m4, m5);
      context.beginPath();
      for (const sub of node.path) {
        context.moveTo(sub.start.x, sub.start.y);
        for (const seg of sub.segs) {
          if (seg.kind === 0) context.lineTo(seg.to.x, seg.to.y);
          else context.bezierCurveTo(seg.c1.x, seg.c1.y, seg.c2.x, seg.c2.y, seg.to.x, seg.to.y);
        }
        if (sub.closed) context.closePath();
      }
      context.fillStyle = `rgba(${r}, ${g}, ${b}, ${a / 255})`;
      context.fill("evenodd");
      if (selected.has(node.id)) {
        context.lineWidth = 1.5 / scale;
        context.strokeStyle = "#f8fafc";
        context.stroke();
      }
      context.restore();
    }
    context.restore();

    // Everything below is drawn in screen pixels so the chrome keeps its size
    // whatever the zoom is.
    if (drag?.kind === "marquee") {
      const { scale: s, offsetX: ox, offsetY: oy } = view();
      const x = Math.min(drag.from.x, drag.to.x) * s + ox;
      const y = Math.min(drag.from.y, drag.to.y) * s + oy;
      const w = Math.abs(drag.to.x - drag.from.x) * s;
      const h = Math.abs(drag.to.y - drag.from.y) * s;
      context.strokeStyle = "#60a5fa";
      context.setLineDash([4, 3]);
      context.strokeRect(x, y, w, h);
      context.setLineDash([]);
    }

    drawGroupOutlines(context, nodes, view());
    drawGuides(context, guides, view());
    if (selectionBox) {
      const box = drag && drag.kind !== "marquee" && drag.command
        ? boxAfterCommand(selectionBox, drag.command)
        : selectionBox;
      drawSelectionFrame(context, box, view());
    }
  }, [drag, guides, height, nodes, selection, selectionBox, view, width]);

  useEffect(() => {
    draw();
    const onResize = (): void => draw();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [draw, revision]);

  /** Starts a handle drag if the pointer is on one (only with a selection). */
  const beginHandleDrag = (screen: { x: number; y: number }): Drag | null => {
    if (!selectionBox) return null;
    const handle = hitHandle(screen, selectionBox, view());
    if (!handle) return null;
    const point = toDocPoint(screen.x, screen.y, view());
    if (handle === "rotate") {
      return { kind: "rotate", box: selectionBox, from: point, command: null };
    }
    return { kind: "scale", handle, box: selectionBox, command: null };
  };

  return (
    <canvas
      ref={canvasRef}
      data-testid="editor-canvas"
      className={`h-full w-full touch-none ${selectionBox ? "cursor-default" : "cursor-crosshair"}`}
      onPointerDown={(event) => {
        const canvas = canvasRef.current;
        const session = editorSession();
        if (!canvas || !session) return;
        canvas.setPointerCapture(event.pointerId);
        const screen = toScreen(event.clientX, event.clientY);
        const handleDrag = beginHandleDrag(screen);
        if (handleDrag) {
          setDrag(handleDrag);
          return;
        }
        const point = toDocPoint(screen.x, screen.y, view());
        const gesture = gestureFor(
          session.pick(point.x, point.y),
          useEditor.getState().selection,
          event.shiftKey,
        );
        if (gesture.kind === "marquee") {
          setDrag({ kind: "marquee", from: point, to: point });
          return;
        }
        // Grabbing an unselected node selects it first, so a drag always moves
        // what is under the cursor (standard direct-manipulation behaviour).
        if (gesture.selectFirst !== "none") {
          const hit = session.pick(point.x, point.y);
          if (gesture.selectFirst === "toggle") session.selectToggle(hit);
          else session.selectOnly(hit);
          useEditor.getState().sync();
        }
        setDrag({ kind: "move", from: point, command: null, moved: false });
      }}
      onPointerMove={(event) => {
        if (!drag) return;
        const screen = toScreen(event.clientX, event.clientY);
        const point = toDocPoint(screen.x, screen.y, view());
        if (drag.kind === "move") {
          const proposedX = point.x - drag.from.x;
          const proposedY = point.y - drag.from.y;
          const moved = drag.moved || isDrag(proposedX, proposedY);
          if (!moved) {
            setDrag({ ...drag, moved });
            return;
          }
          const snapped = snapMove(proposedX, proposedY);
          const command: EditorCommand = { kind: "translate", dx: snapped.dx, dy: snapped.dy };
          preview(command);
          setDrag({ ...drag, moved, command });
          return;
        }
        if (drag.kind === "scale") {
          const command = scaleCommandFor(drag.handle, drag.box, point, {
            uniform: !event.shiftKey,
            fromCenter: event.altKey,
          });
          if (command) preview(command);
          setDrag({ ...drag, command });
          return;
        }
        if (drag.kind === "rotate") {
          const command = rotateCommandFor(drag.box, drag.from, point, { snap: event.shiftKey });
          if (command) preview(command);
          setDrag({ ...drag, command });
          return;
        }
        setDrag({ ...drag, to: point });
      }}
      onPointerUp={(event) => {
        if (!drag) return;
        const point = toDoc(event.clientX, event.clientY);
        // One gesture, one command: committing the same spec the preview showed
        // is what makes the history exactly one step deep.
        if (drag.kind !== "marquee" && drag.command) {
          commit(drag.command);
        } else if (drag.kind === "move") {
          click(point.x, point.y, event.shiftKey);
        } else if (drag.kind === "marquee") {
          const box = marqueeBox(drag.from, point);
          if (box.x1 - box.x0 < 1 && box.y1 - box.y0 < 1) {
            click(point.x, point.y, event.shiftKey);
          } else {
            marquee(box);
          }
        } else {
          // A handle drag that never moved anything: nothing to commit.
          useEditor.getState().cancelPreview();
        }
        setGuides([]);
        setDrag(null);
      }}
      onPointerCancel={() => {
        useEditor.getState().cancelPreview();
        setGuides([]);
        setDrag(null);
      }}
    />
  );
}

/** A dashed outline around every group, so membership is visible at a glance. */
function drawGroupOutlines(
  context: CanvasRenderingContext2D,
  nodes: readonly NodeSpec[],
  view: View,
): void {
  const members = new Map<number, Box>();
  for (const node of nodes) {
    if (!node.group) continue;
    const box = nodeBox(node);
    const current = members.get(node.group);
    members.set(
      node.group,
      current
        ? {
            x0: Math.min(current.x0, box.x0),
            y0: Math.min(current.y0, box.y0),
            x1: Math.max(current.x1, box.x1),
            y1: Math.max(current.y1, box.y1),
          }
        : box,
    );
  }
  if (members.size === 0) return;
  context.save();
  context.strokeStyle = "#a78bfa";
  context.lineWidth = 1;
  context.setLineDash([5, 3]);
  for (const box of members.values()) {
    const a = toScreenPoint(box.x0, box.y0, view);
    const b = toScreenPoint(box.x1, box.y1, view);
    context.strokeRect(a.x - 3, a.y - 3, b.x - a.x + 6, b.y - a.y + 6);
  }
  context.restore();
}

/** The placed box of one node (its local path box through its own affine). */
function nodeBox(node: NodeSpec): Box {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  const [a, b, c, d, e, f] = node.m;
  const put = (x: number, y: number): void => {
    const px = a * x + c * y + e;
    const py = b * x + d * y + f;
    x0 = Math.min(x0, px);
    y0 = Math.min(y0, py);
    x1 = Math.max(x1, px);
    y1 = Math.max(y1, py);
  };
  for (const sub of node.path) {
    put(sub.start.x, sub.start.y);
    for (const seg of sub.segs) {
      if (seg.kind === 0) put(seg.to.x, seg.to.y);
      else {
        // Control points bound a cubic loosely; the engine's own box is exact,
        // and this one only frames groups, so a loose box is honest enough.
        put(seg.c1.x, seg.c1.y);
        put(seg.c2.x, seg.c2.y);
        put(seg.to.x, seg.to.y);
      }
    }
  }
  if (!Number.isFinite(x0)) return { x0: 0, y0: 0, x1: 0, y1: 0 };
  return { x0, y0, x1, y1 };
}

/** The selection box, its eight scale handles and its rotate grip. */
function drawSelectionFrame(context: CanvasRenderingContext2D, box: Box, view: View): void {
  const nw = toScreenPoint(box.x0, box.y0, view);
  const se = toScreenPoint(box.x1, box.y1, view);
  const handles = handlePositions(box, view);
  context.save();
  context.strokeStyle = "#60a5fa";
  context.lineWidth = 1;
  context.strokeRect(nw.x, nw.y, se.x - nw.x, se.y - nw.y);
  // The grip hangs off the top edge on a short stalk.
  context.beginPath();
  context.moveTo(handles.n.x, handles.n.y);
  context.lineTo(handles.rotate.x, handles.rotate.y);
  context.stroke();
  context.fillStyle = "#f8fafc";
  for (const [id, point] of Object.entries(handles) as Array<[HandleId, { x: number; y: number }]>) {
    if (id === "rotate") {
      context.beginPath();
      context.arc(point.x, point.y, HANDLE_RADIUS, 0, Math.PI * 2);
      context.fill();
      context.stroke();
      continue;
    }
    context.fillRect(point.x - HANDLE_RADIUS, point.y - HANDLE_RADIUS, HANDLE_RADIUS * 2, HANDLE_RADIUS * 2);
  }
  context.restore();
}

/** The guides a snap answered with: thin lines across the canvas. */
function drawGuides(
  context: CanvasRenderingContext2D,
  guides: readonly SnapGuide[],
  view: View,
): void {
  if (guides.length === 0) return;
  context.save();
  context.strokeStyle = "#f472b6";
  context.lineWidth = 1;
  context.setLineDash([3, 3]);
  for (const guide of guides) {
    if (guide.axis === 0) {
      const top = toScreenPoint(guide.position, guide.from, view);
      const bottom = toScreenPoint(guide.position, guide.to, view);
      context.beginPath();
      context.moveTo(top.x, top.y);
      context.lineTo(bottom.x, bottom.y);
    } else {
      const left = toScreenPoint(guide.from, guide.position, view);
      const right = toScreenPoint(guide.to, guide.position, view);
      context.beginPath();
      context.moveTo(left.x, left.y);
      context.lineTo(right.x, right.y);
    }
    context.stroke();
  }
  context.restore();
}
