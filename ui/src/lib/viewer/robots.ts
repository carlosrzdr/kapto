/**
 * The robots the view draws: the one Kapto runs, loaded once, and the one
 * the episode replayed was recorded with, kept while it is replayed, so
 * going back and forth between them loads neither again.
 */
export class Robots<R> {
  private live: Promise<R | null> | null = null;
  private replayed: { id: string; robot: Promise<R | null> } | null = null;

  /** `load(null)` loads the live robot, `load(id)` replay `id`'s. */
  constructor(private readonly load: (id: string | null) => Promise<R | null>) {}

  /** The live robot for `null`, or replay `id`'s; `null` if it has none. */
  of(id: string | null): Promise<R | null> {
    if (id === null) {
      this.live ??= this.kept(this.load(null), () => (this.live = null));
      return this.live;
    }
    if (this.replayed?.id !== id) {
      const robot = this.kept(this.load(id), () => {
        if (this.replayed?.robot === robot) {
          this.replayed = null;
        }
      });
      this.replayed = { id, robot };
    }
    return this.replayed.robot;
  }

  /** `loading`, forgotten if it fails, so the next time loads it again. */
  private kept(loading: Promise<R | null>, forget: () => void): Promise<R | null> {
    return loading.catch((error: unknown) => {
      forget();
      throw error;
    });
  }
}
