import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { AuthProvider } from './lib/auth';
import { StoreProvider } from './lib/store';
import { initWasm } from './lib/wasm';
import './styles.css';

const root = createRoot(document.getElementById('root')!);

initWasm()
  .then(() =>
    root.render(
      <StrictMode>
        <AuthProvider>
          <StoreProvider>
            <App />
          </StoreProvider>
        </AuthProvider>
      </StrictMode>,
    ),
  )
  .catch((e) => {
    root.render(<pre style={{ padding: 24 }}>Failed to load the WebAssembly wallet: {String(e)}</pre>);
  });
