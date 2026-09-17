import { useCallback, useEffect, useRef, useState } from "react";

import { editorSession, useEditor } from "../../state/editorStore";
import { fitView, gestureFor, isDrag, marqueeBox, toDocPoint, type View } from "./canvasModel";

/** A drag in progress: either moving the selection or sweeping a marquee. */
type Drag =
  | { kind: "move"; from: { x: number; y: number }; dx: number; dy: number; moved: boolean }
  | { kind: "marquee"; from: { x: number; y: number }; to: { x: number; y: number } };

/** Margin around the document when it is fitted into the canvas. */
const MARGIN = 12;

/**
 * The editing surface.
 *
 * It draws straight from the session's node list and turns every pointer gesture
 * into exactly one command when the gesture *ends*: dragging across the canvas
 * never round-trips to Rust, and one drag is one undo step rather than one per
 * frame (ARCHITECTURE §2).
 */
export function EditorCanvas() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const nodes = useEditor((s) => s.nodes);
  const selection = useEditor((s) => s.selection);
  const revision = useEditor((s) => s.revision);
  const width = useEditor((s) => s.width);
  const height = useEditor((s) => s.height);
  const click = useEditor((s) => s.click);
  const marquee = useEditor((s) => s.marquee);
  const nudge = useEditor((s) => s.nudge);
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
  }, [drag, height, nodes, selection, view, width]);

  useEffect(() => {
    draw();
    const onResize = (): void => draw();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [draw, revision]);

  return (
    <canvas
      ref={canvasRef}
      data-testid="editor-canvas"
      className="h-full w-full cursor-crosshair touch-none"
      onPointerDown={(event) => {
        const canvas = canvasRef.current;
        const session = editorSession();
        if (!canvas || !session) return;
        canvas.setPointerCapture(event.pointerId);
        const point = toDoc(event.clientX, event.clientY);
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
        setDrag({ kind: "move", from: point, dx: 0, dy: 0, moved: false });
      }}
      onPointerMove={(event) => {
        if (!drag) return;
        const point = toDoc(event.clientX, event.clientY);
        if (drag.kind === "move") {
          const dx = point.x - drag.from.x;
          const dy = point.y - drag.from.y;
          setDrag({ ...drag, dx, dy, moved: drag.moved || isDrag(dx, dy) });
        } else {
          setDrag({ ...drag, to: point });
        }
      }}
      onPointerUp={(event) => {
        if (!drag) return;
        const point = toDoc(event.clientX, event.clientY);
        if (drag.kind === "move") {
          if (drag.moved) nudge(drag.dx, drag.dy);
          else click(point.x, point.y, event.shiftKey);
        } else {
          const box = marqueeBox(drag.from, point);
          if (box.x1 - box.x0 < 1 && box.y1 - box.y0 < 1) {
            click(point.x, point.y, event.shiftKey);
          } else {
            marquee(box);
          }
        }
        setDrag(null);
      }}
      onPointerCancel={() => setDrag(null)}
    />
  );
}
