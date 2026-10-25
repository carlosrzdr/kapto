import type { Locator } from "@playwright/test";

/**
 * How much of the live view's 3D picture differs from its background, from
 * 0 to 1: a robot and the floor's grid drawn make it more than a few
 * percent, a picture left empty none. Read from a screenshot of the canvas,
 * as WebGL keeps no picture to read back once it is shown; below its top
 * fifth, where the view's own buttons and words lie over it.
 */
export async function drawnShare(live: Locator): Promise<number> {
  const canvas = live.locator("canvas");
  const png = (await canvas.screenshot({ animations: "disabled" })).toString("base64");
  return canvas.evaluate(async (_, data) => {
    // An image, as the page's own rules let it load one from data but not fetch it.
    const picture = new Image();
    picture.src = `data:image/png;base64,${data}`;
    await picture.decode();
    const scratch = new OffscreenCanvas(picture.width, picture.height);
    const context = scratch.getContext("2d");
    if (!context) {
      return 0;
    }
    context.drawImage(picture, 0, 0);
    const top = Math.floor(picture.height / 5);
    const { data: pixels } = context.getImageData(0, top, picture.width, picture.height - top);
    // The background is what the bottom left corner shows: the floor never reaches it.
    const at = pixels.length - picture.width * 4;
    const background = [pixels[at]!, pixels[at + 1]!, pixels[at + 2]!];
    let differing = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      if (background.some((value, channel) => Math.abs(pixels[i + channel]! - value) > 16)) {
        differing += 1;
      }
    }
    return differing / (pixels.length / 4);
  }, png);
}
