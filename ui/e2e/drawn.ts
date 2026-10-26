import type { Locator } from "@playwright/test";

/**
 * A screenshot of a robot panel, as PNG in base64: the panel as the page
 * shows it, as the scene's canvas lies under the whole view and each robot
 * panel shows its own part of it; and WebGL keeps no picture to read back
 * once it is shown.
 */
export async function pictureOf(live: Locator): Promise<string> {
  return (await live.screenshot({ animations: "disabled" })).toString("base64");
}

/**
 * Compares two pictures of a panel in the page, below their top fifth,
 * where the view's own buttons and words lie over it: the share of pixels
 * that differ from `before`'s, or, without `before`, from the background
 * its bottom left corner shows, which the floor never reaches.
 */
function compared(live: Locator, now: string, before: string | null): Promise<number> {
  return live.evaluate(
    async (_, [now, before]) => {
      // Images, as the page's own rules let it load one from data but not fetch it.
      const read = async (data: string) => {
        const picture = new Image();
        picture.src = `data:image/png;base64,${data}`;
        await picture.decode();
        const scratch = new OffscreenCanvas(picture.width, picture.height);
        const context = scratch.getContext("2d");
        if (!context) {
          return null;
        }
        context.drawImage(picture, 0, 0);
        const top = Math.floor(picture.height / 5);
        return { pixels: context.getImageData(0, top, picture.width, picture.height - top).data, width: picture.width };
      };
      const shown = await read(now!);
      const earlier = before ? await read(before) : null;
      if (!shown || (before && (!earlier || earlier.pixels.length !== shown.pixels.length))) {
        return before ? 1 : 0;
      }
      const { pixels, width } = shown;
      const at = pixels.length - width * 4;
      const background = [pixels[at]!, pixels[at + 1]!, pixels[at + 2]!];
      let differing = 0;
      for (let i = 0; i < pixels.length; i += 4) {
        const against = (channel: number) => (earlier ? earlier.pixels[i + channel]! : background[channel]!);
        if ([0, 1, 2].some((channel) => Math.abs(pixels[i + channel]! - against(channel)) > 16)) {
          differing += 1;
        }
      }
      return differing / (pixels.length / 4);
    },
    [now, before],
  );
}

/**
 * How much of a robot panel's 3D picture differs from its background, from
 * 0 to 1: a robot and the floor's grid drawn make it more than a few
 * percent, a picture left empty none.
 */
export async function drawnShare(live: Locator): Promise<number> {
  return compared(live, await pictureOf(live), null);
}

/** How much of a robot panel's 3D picture changed since `before` (from `pictureOf`), from 0 to 1. */
export async function changedShare(live: Locator, before: string): Promise<number> {
  return compared(live, await pictureOf(live), before);
}
