import { formatName } from 'acme-sdk';
import * as fromAuth from '@app/auth/reducers';
import Widget from 'not-a-dependency';
import * as fs from 'fs';
import * as rx from 'rxjs';

export function usePackage(): void {
  formatName('z');
  fromAuth.selectUser();
  Widget.render();
  fs.readFileSync('x');
  rx.of(1);
}
