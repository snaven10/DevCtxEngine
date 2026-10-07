import express from 'express';
import { route } from './routes.js';

export const start = (port) => {
  listen(port);
};

function boot() {
  start(80);
}

export class App extends Base {
  constructor() {
    super();
    this.server = new Server();
    this.onTick = () => this.tick();
  }

  run() {
    boot();
  }

  count = 0;

  stop = () => {
    this.server.close();
  };
}

exports.handler = function () {
  work();
};

const onDone = function finish() {
  notify();
};

export default boot;
