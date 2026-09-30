import { Component } from 'react';
import type { ReactNode } from 'react';
export class ErrorBoundary extends Component<{ children: ReactNode; onError: (error: Error) => void }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError(): { failed: boolean } { return { failed: true }; }
  componentDidCatch(error: Error): void { this.props.onError(error); }
  render(): ReactNode { return this.state.failed ? <div className="app theme-dark"><div className="empty-state"><strong>The workspace view encountered an error.</strong><div>Your repository files have not been changed by this display failure.</div><button onClick={() => this.setState({ failed: false })}>Retry workspace view</button></div></div> : this.props.children; }
}
