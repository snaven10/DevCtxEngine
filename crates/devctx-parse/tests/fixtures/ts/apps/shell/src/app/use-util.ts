import { formatName } from './util';
import * as util from './util';
import DefaultStore from './store';
import { formatName as fromPackage } from 'acme-sdk';
import { of } from 'rxjs';
import { tokenInterceptor } from '@acme/auth';

export function run(): void {
  formatName('x');
  util.helper();
  new DefaultStore().clear();
  fromPackage('y');
  of(1);
  tokenInterceptor(new Request('u'), (r) => r);
}
