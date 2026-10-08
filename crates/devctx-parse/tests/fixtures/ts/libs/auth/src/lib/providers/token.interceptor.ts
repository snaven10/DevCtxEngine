import { inject } from '@angular/core';
import { AuthService } from '../services/auth.service';

export const tokenInterceptor = (req: Request, next: (r: Request) => Request) => {
  const s = inject(AuthService);
  s.token();
  return next(req);
};
