import { greet } from './helpers';
import express from 'express';

export class Main {
  run(svc) {
    this.go();
    greet('x');
    svc.vanishing();
    express().listen(80);
  }

  go() {}
}
