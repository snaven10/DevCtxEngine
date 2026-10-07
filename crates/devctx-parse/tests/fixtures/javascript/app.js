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
  }

  run() {
    boot();
  }

  stop = () => {
    this.server.close();
  };
}
