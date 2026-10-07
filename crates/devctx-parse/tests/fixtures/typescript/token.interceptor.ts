import { Injectable, inject } from '@angular/core';
import { HttpInterceptorFn as Fn } from '@angular/common/http';
import * as rx from 'rxjs';
import AuthStore from './auth.store';
import './polyfills';

export const tokenInterceptor: Fn = (req, next) => {
  const store = inject(AuthStore);
  return next(req.clone({ headers: store.headers() }));
};

const helper = function () {
  return log('x');
};

export interface Session extends Base<string> {
  token: string;
}

export type Token = string;

export enum Mode { A, B }

@Injectable()
export class AuthService extends BaseService implements OnInit, Auditable {
  private cache: Map<string, Session> = new Map();

  constructor(private readonly http: HttpClient, store: AuthStore) {
    super();
  }

  ngOnInit(): void {
    this.load();
    [1, 2].map((x) => audit(x));
  }

  handle = (e: Event) => {
    this.load();
  };

  private load(): Session {
    return new SessionImpl();
  }
}

register(tokenInterceptor);
