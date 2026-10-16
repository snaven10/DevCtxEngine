export function leaf() {
  return 1;
}

export class Widget {
  render() {
    const cb = () => leaf();
    return this.paint(cb());
  }

  paint(x) {
    return helper.draw(x);
  }
}

function outer() {
  function inner() {
    return leaf();
  }
  return inner();
}
