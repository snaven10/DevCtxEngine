import { inject } from '@angular/core';
import { AuthService as DataAuth } from '@acme/auth-data';

export class DataComponent {
  private readonly auth = inject(DataAuth);

  load(): string {
    return this.auth.token();
  }
}
