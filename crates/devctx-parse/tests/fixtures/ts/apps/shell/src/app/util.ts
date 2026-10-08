export const formatName = (n: string): string => n.trim();

export function helper(): void {
  console.log(JSON.stringify({}));
}

export function stop(): void {}

export const api = {
  run() {
    this.stop();
  },
  stop() {},
};

export const Klass = class {
  m() {
    this.n();
  }
  n() {}
};
