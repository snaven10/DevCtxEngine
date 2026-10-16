import { formatName } from 'acme-sdk';
import * as fromAuth from '@app/auth/reducers';
import Widget from 'not-a-dependency';
import * as fs from 'fs';
import * as rx from 'rxjs';
import { helper } from 'unlisted-sdk';
import { filter } from 'rxjs';

export function usePackage(): void {
  formatName('z');
  fromAuth.selectUser();
  Widget.render();
  fs.readFileSync('x');
  rx.of(1);
  helper();
  filter();
}
