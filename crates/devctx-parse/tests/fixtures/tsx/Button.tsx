import React from 'react';

export const Button = (props: Props) => {
  const onClick = () => track('click');
  return <button onClick={onClick}>{props.label}</button>;
};

export function Panel(): JSX.Element {
  return <Button label="x" />;
}
