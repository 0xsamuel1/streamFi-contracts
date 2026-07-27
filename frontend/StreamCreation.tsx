import React, { useState } from 'react';
import { validateStreamPayload } from './lib/validateStreamPayload';

const RPC_TIMEOUT_MS = 10_000;

interface CreateStreamPayload {
  recipient: string;
  amount: number;
  ratePerSecond: number;
}

async function createStreamOnChain(payload: CreateStreamPayload): Promise<{ streamId: string }> {
  const response = await fetch('/api/streams', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });

  if (!response.ok) {
    throw new Error(`Stream creation failed with status ${response.status}`);
  }

  return response.json();
}

export const StreamCreation: React.FC = () => {
  const [recipient, setRecipient] = useState('');
  const [amount, setAmount] = useState('');
  const [ratePerSecond, setRatePerSecond] = useState('');
  const [loading, setLoading] = useState(false);
  const [validationErrors, setValidationErrors] = useState<string[]>([]);

  const validateCurrentInputs = () => {
    const result = validateStreamPayload({
      recipient,
      amount: Number(amount),
      ratePerSecond: Number(ratePerSecond),
    });
    setValidationErrors(result.errors);
  };

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    setValidationErrors([]);

    const payload = { recipient, amount: Number(amount), ratePerSecond: Number(ratePerSecond) };
    const result = validateStreamPayload(payload);
    if (!result.valid) {
      setValidationErrors(result.errors);
      return;
    }

    setLoading(true);

    // FIX for Bug #150: the previous implementation awaited the RPC call
    // with no timeout, so if the RPC provider never responded, `loading`
    // was never reset and the spinner spun forever. Racing the request
    // against a timeout guarantees the loading state always clears, either
    // with a result or a clear timeout error.
    const timeout = new Promise<never>((_, reject) => {
      setTimeout(() => reject(new Error('RPC provider timed out. Please try again.')), RPC_TIMEOUT_MS);
    });

    try {
      await Promise.race([createStreamOnChain(payload), timeout]);
    } catch (e) {
      setValidationErrors([e instanceof Error ? e.message : 'Failed to create stream.']);
    } finally {
      setLoading(false);
    }
  };

  return (
    <form className="stream-creation" onSubmit={handleSubmit}>
      <h2>Create Stream</h2>

      <label>
        Recipient address
        <input value={recipient} onChange={(e) => { setRecipient(e.target.value); validateCurrentInputs(); }} />
      </label>

      <label>
        Amount
        <input value={amount} onChange={(e) => { setAmount(e.target.value); validateCurrentInputs(); }} />
      </label>

      <label>
        Rate per second
        <input value={ratePerSecond} onChange={(e) => { setRatePerSecond(e.target.value); validateCurrentInputs(); }} />
      </label>

      {validationErrors.length > 0 && (
        <ul className="validation-errors">
          {validationErrors.map((err) => (
            <li key={err}>{err}</li>
          ))}
        </ul>
      )}

      <button type="submit" disabled={loading || validationErrors.length > 0}>
        {loading ? 'Creating stream...' : 'Create stream'}
      </button>
    </form>
  );
};