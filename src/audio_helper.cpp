// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <audiopolicy.h>
#include <wrl/client.h>
#include <atomic>
#include <memory>

// This helper provides native WASAPI status monitoring to correctly detect
// when the audio device is locked by an exclusive-mode application.

using Microsoft::WRL::ComPtr;

// RAII Guard for COM initialization
class ComInitGuard {
    HRESULT m_hr;
public:
    ComInitGuard(DWORD coinit = COINIT_MULTITHREADED) {
        m_hr = CoInitializeEx(nullptr, coinit);
    }
    ~ComInitGuard() {
        if (SUCCEEDED(m_hr)) {
            CoUninitialize();
        }
    }
    bool Succeeded() const { return SUCCEEDED(m_hr); }
};

// Minimal COM implementation of IAudioSessionEvents
class AudioSessionEvents : public IAudioSessionEvents {
    LONG _refCount;
    std::atomic<bool>* m_signal;
public:
    AudioSessionEvents(std::atomic<bool>* signal) : _refCount(1), m_signal(signal) {}
    virtual ~AudioSessionEvents() {}
    
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override {
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IAudioSessionEvents)) {
            *ppv = static_cast<IAudioSessionEvents*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    
    STDMETHODIMP_(ULONG) AddRef() override { 
        return InterlockedIncrement(&_refCount); 
    }
    
    STDMETHODIMP_(ULONG) Release() override {
        ULONG res = InterlockedDecrement(&_refCount);
        if (res == 0) delete this;
        return res;
    }

    // Unused IAudioSessionEvents methods
    STDMETHODIMP OnDisplayNameChanged(LPCWSTR, LPCGUID) override { return S_OK; }
    STDMETHODIMP OnIconPathChanged(LPCWSTR, LPCGUID) override { return S_OK; }
    STDMETHODIMP OnSimpleVolumeChanged(float, BOOL, LPCGUID) override { return S_OK; }
    STDMETHODIMP OnChannelVolumeChanged(DWORD, float[], DWORD, LPCGUID) override { return S_OK; }
    STDMETHODIMP OnGroupingParamChanged(LPCGUID, LPCGUID) override { return S_OK; }
    STDMETHODIMP OnStateChanged(AudioSessionState) override { return S_OK; }

    STDMETHODIMP OnSessionDisconnected(AudioSessionDisconnectReason reason) override {
        if (reason == DisconnectReasonExclusiveModeOverride || reason == DisconnectReasonDeviceRemoval) {
            if (m_signal) m_signal->store(true);
        }
        return S_OK;
    }
};

class AudioMonitor {
    ComInitGuard m_comGuard;
    ComPtr<IMMDevice> m_device;
    ComPtr<IAudioSessionControl> m_sessionControl;
    ComPtr<AudioSessionEvents> m_listener;
    std::atomic<bool> m_deviceLost;

public:
    AudioMonitor() : m_deviceLost(false) {
        if (!m_comGuard.Succeeded()) return;

        ComPtr<IMMDeviceEnumerator> enumerator;
        if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&enumerator)))) {
            return;
        }

        if (FAILED(enumerator->GetDefaultAudioEndpoint(eRender, eConsole, &m_device))) {
            return;
        }

        ComPtr<IAudioSessionManager2> manager;
        if (FAILED(m_device->Activate(__uuidof(IAudioSessionManager2), CLSCTX_ALL, nullptr, &manager))) {
            return;
        }

        if (FAILED(manager->GetAudioSessionControl(nullptr, 0, &m_sessionControl))) {
            return;
        }

        m_listener = new AudioSessionEvents(&m_deviceLost);
        // Register our listener to get notified about session events (like disconnection)
        m_sessionControl->RegisterAudioSessionNotification(m_listener.Get());
    }

    ~AudioMonitor() {
        if (m_sessionControl && m_listener) {
            m_sessionControl->UnregisterAudioSessionNotification(m_listener.Get());
        }
        // ComPtrs and ComInitGuard handle the rest automatically
    }

    bool IsDeviceAvailable() {
        if (!m_comGuard.Succeeded() || !m_device) return false;

        ComPtr<IAudioClient> client;
        if (FAILED(m_device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, &client))) {
            return false;
        }

        WAVEFORMATEX* rawFormat = nullptr;
        if (FAILED(client->GetMixFormat(&rawFormat))) {
            return false;
        }

        // RAII Guard for CoTaskMem memory
        std::unique_ptr<WAVEFORMATEX, void(*)(void*)> format(rawFormat, [](void* p) { CoTaskMemFree(p); });

        // Shared mode initialization check
        HRESULT hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 0, 0, format.get(), nullptr);
        return SUCCEEDED(hr);
    }

    bool PollDeviceLost() {
        return m_deviceLost.exchange(false);
    }
    
    bool IsInitialized() const {
        return m_sessionControl != nullptr;
    }
};

static std::unique_ptr<AudioMonitor> g_monitor = nullptr;

extern "C" {
    void wasapi_monitor_init() {
        if (!g_monitor) {
            g_monitor = std::make_unique<AudioMonitor>();
            if (!g_monitor->IsInitialized()) {
                g_monitor.reset();
            }
        }
    }

    void wasapi_monitor_uninit() {
        g_monitor.reset();
    }

    bool wasapi_is_device_available() {
        if (g_monitor) {
            return g_monitor->IsDeviceAvailable();
        }
        // Fallback or if common thread init failed
        return false;
    }

    bool wasapi_poll_device_lost() {
        if (g_monitor) {
            return g_monitor->PollDeviceLost();
        }
        return false;
    }
}
