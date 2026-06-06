// What the browser has and jsdom lacks, for the component tests.

// Svelte's size bindings watch an element's size; jsdom lays nothing out,
// so nothing is ever seen to change.
globalThis.ResizeObserver ??= class {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
};
