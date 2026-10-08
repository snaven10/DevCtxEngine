import { Injectable } from '@angular/core';
import { Observable, of } from 'rxjs';

export interface Session {
  token: string;
}

@Injectable({ providedIn: 'root' })
export class AuthService {
  token(): string {
    return 'x';
  }

  login(user: string): Observable<Session> {
    return of({ token: user });
  }

  refresh(): Session {
    return { token: this.token() };
  }
}
