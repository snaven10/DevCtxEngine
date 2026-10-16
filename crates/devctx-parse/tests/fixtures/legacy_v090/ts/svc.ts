export function leaf(): number {
  return 1;
}

export class C {
  field = () => leaf();

  m(): number {
    const cb = () => leaf();
    return cb();
  }

  m2(): number {
    const fe = function () {
      return leaf();
    };
    return fe();
  }

  m3(): number {
    function inner(): number {
      return leaf();
    }
    return inner();
  }

  m4(): number {
    const fe2 = function named() {
      return leaf();
    };
    return fe2();
  }

  run(): number {
    return this.m();
  }

  run4(): number {
    return this.m4();
  }

  runField(): number {
    return this.field();
  }

  run2(): number {
    return this.m2();
  }

  run3(): number {
    return this.m3();
  }
}
