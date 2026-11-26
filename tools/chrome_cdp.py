"""Small Chrome DevTools pipe helper for real-time rendering regression oracles."""
import json
import os
import select
import signal
import subprocess
import tempfile
import time


def capture_page(url, settle_ms=1500):
    # Real wall time matters: virtual-time dump-DOM can close before image LCP.
    with tempfile.TemporaryDirectory(prefix='ps-cdp-') as profile:
        request_read, request_write = os.pipe()
        response_read, response_write = os.pipe()
        process = None
        call = None
        try:
            process = subprocess.Popen([
                'bash', '-c', 'exec 3<&"$1" 4>&"$2"; shift 2; exec "$@"', 'chrome-pipe',
                str(request_read), str(response_write), 'google-chrome', '--headless',
                '--no-sandbox', '--disable-gpu', '--no-proxy-server',
                '--remote-debugging-pipe', '--window-size=800,600',
                '--user-data-dir=' + profile, 'about:blank'],
                pass_fds=(request_read, response_write), start_new_session=True, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL)
            os.close(request_read)
            request_read = None
            os.close(response_write)
            response_write = None
            buffer = b''
            sequence = 0

            def call(method, params=None, session=None):
                nonlocal sequence, buffer
                sequence += 1
                request = {'id': sequence, 'method': method, 'params': params or {}}
                if session:
                    request['sessionId'] = session
                payload = json.dumps(request).encode() + b'\0'
                while payload:
                    payload = payload[os.write(request_write, payload):]
                deadline = time.monotonic() + 15
                while True:
                    while b'\0' in buffer:
                        packet, buffer = buffer.split(b'\0', 1)
                        if not packet:
                            continue
                        message = json.loads(packet)
                        if message.get('id') == sequence:
                            if 'error' in message:
                                raise RuntimeError(message['error'])
                            return message.get('result', {})
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not select.select([response_read], [], [], remaining)[0]:
                        raise TimeoutError(method)
                    chunk = os.read(response_read, 65536)
                    if not chunk:
                        raise RuntimeError('Chrome DevTools pipe closed')
                    buffer += chunk

            target = call('Target.createTarget', {'url': 'about:blank'})['targetId']
            session = call('Target.attachToTarget', {'targetId': target, 'flatten': True})['sessionId']
            call('Page.enable', session=session)
            call('Page.navigate', {'url': url}, session)
            deadline = time.monotonic() + 15
            while True:
                try:
                    state = call('Runtime.evaluate', {
                        'expression': 'location.href === ' + json.dumps(url) + ' && document.readyState === "complete"',
                        'returnByValue': True}, session)
                    if state.get('result', {}).get('value'):
                        break
                except RuntimeError as error:
                    if 'context' not in str(error).lower():
                        raise
                if time.monotonic() >= deadline:
                    raise TimeoutError('page load')
                time.sleep(.05)
            expression = '''new Promise(resolve => setTimeout(() => resolve({
                html: document.documentElement.outerHTML,
                beacon: window.pagespeed && pagespeed.CriticalImages ? pagespeed.CriticalImages.getBeaconData() : ''
            }), DELAY))'''.replace('DELAY', str(int(settle_ms)))
            response = call('Runtime.evaluate', {
                'expression': expression, 'awaitPromise': True, 'returnByValue': True}, session)
            if 'exceptionDetails' in response:
                raise RuntimeError(response['exceptionDetails'])
            return response['result']['value']
        finally:
            if process:
                if call is not None and process.poll() is None:
                    try:
                        call('Browser.close')
                    except (RuntimeError, TimeoutError, OSError):
                        pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)
            for descriptor in [request_read, request_write, response_read, response_write]:
                if descriptor is not None:
                    os.close(descriptor)
