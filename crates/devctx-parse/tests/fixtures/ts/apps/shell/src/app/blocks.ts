export function cb(): void {}

export function outer(): void {
  {
    const cb = (): void => {};
    cb();
  }
  cb();
  {
    const cb = (): void => {};
    cb();
  }
}
