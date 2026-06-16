// What the browser has and jsdom lacks, for the component tests.

// Svelte's size bindings watch an element's size; jsdom lays nothing out,
// so nothing is ever seen to change.
globalThis.ResizeObserver ??= class {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
};

// uPlot asks for the screen's pixel ratio as it loads, through media
// queries jsdom doesn't answer.
globalThis.matchMedia ??= (query: string) =>
  ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  }) as MediaQueryList;
