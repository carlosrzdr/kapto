// How wide the page draws each camera, said to the bridge, which encodes a
// raw camera's pictures no wider than the widest page draws it. Said once
// the panels have settled, a moment after the last change, each width
// rounded up to a step, so dragging an edge doesn't ask once per movement.

/** Widths are rounded up to this many device pixels. */
export const WIDTH_STEP = 128;

/** How long after a panel's last change the widths are said, in milliseconds. */
export const SETTLED_MS = 200;

/** How wide the page draws one camera, in device pixels: 0 if it doesn't. */
export interface CameraWidth {
  stream: number;
  width: number;
}

/** The camera panels' widths, said to the bridge as they settle. */
export class CameraWidths {
  /** Each panel's camera and width, by the panel. */
  private readonly panels = new Map<object, CameraWidth>();
  /** The live stream's cameras, by their position among its streams. */
  private cameras: number[] = [];
  private timer: ReturnType<typeof setTimeout> | undefined;
  /** What was said last, so the same isn't said again. */
  private said = "";

  constructor(
    private readonly say: (cameras: CameraWidth[]) => void,
    private readonly settledMs = SETTLED_MS,
  ) {}

  /** A new connection's cameras: everything is said to it afresh. */
  connected(cameras: number[]) {
    this.cameras = cameras;
    this.said = "";
    this.settle();
  }

  /** Panel `panel` draws camera `stream` `width` device pixels wide; 0 once it is gone. */
  drawn(panel: object, stream: number, width: number) {
    if (width > 0) {
      this.panels.set(panel, { stream, width });
    } else {
      this.panels.delete(panel);
    }
    this.settle();
  }

  /** Each camera's width as said: its widest panel's, rounded up to a step. */
  widths(): CameraWidth[] {
    return this.cameras.map((stream) => {
      let widest = 0;
      for (const panel of this.panels.values()) {
        if (panel.stream === stream) {
          widest = Math.max(widest, panel.width);
        }
      }
      return { stream, width: Math.ceil(widest / WIDTH_STEP) * WIDTH_STEP };
    });
  }

  /** Stops saying anything. */
  stop() {
    clearTimeout(this.timer);
  }

  private settle() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      const widths = this.widths();
      const said = JSON.stringify(widths);
      if (widths.length > 0 && said !== this.said) {
        this.said = said;
        this.say(widths);
      }
    }, this.settledMs);
  }
}
