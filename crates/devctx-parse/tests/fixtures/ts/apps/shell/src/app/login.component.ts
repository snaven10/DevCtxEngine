import { Component } from '@angular/core';
import { map } from 'rxjs/operators';
import { AuthService } from '@acme/auth';
import { Store } from './store';

@Component({ selector: 'acme-login' })
export class LoginComponent {
  constructor(
    private readonly auth: AuthService,
    store: Store,
  ) {
    store.save();
  }

  submit(): void {
    this.auth.login('me').pipe(map((s) => s.token));
  }

  shadow(auth: unknown): void {
    (auth as { token(): void }).token();
  }

  callback(): void {
    [1].forEach((auth) => auth.token());
    try {
      this.auth.token();
    } catch (auth) {
      auth.token();
    }
  }
}
