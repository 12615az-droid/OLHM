/* Test-only NVML ABI fixture. No hardware access or control functions. */
#include <stdint.h>
#include <string.h>
typedef void *Device;
typedef struct { uint64_t total, free, used; } Memory;
typedef struct { unsigned gpu, memory; } Util;
typedef struct { unsigned version, fan, speed; } Fan;
typedef struct { unsigned id, scope; int64_t timestamp, latency; unsigned type; int status; uint64_t value; } Field;
int nvmlInit_v2(void) { return 0; }
int nvmlShutdown(void) { return 0; }
int nvmlSystemGetDriverVersion(char *s, unsigned n) { if (n < 8) return 7; strcpy(s,"999.123"); return 0; }
int nvmlDeviceGetHandleByPciBusId_v2(const char *pci, Device *d) { if (strcmp(pci,"0000:01:00.0")) return 4; *d=(Device)(uintptr_t)1; return 0; }
int nvmlDeviceGetName(Device d, char *s, unsigned n) { if(n<16)return 7; strcpy(s,"Fixture NVIDIA"); return 0; }
int nvmlDeviceGetPowerUsage(Device d, unsigned *n) { *n=75000; return 0; }
int nvmlDeviceGetPowerManagementLimit(Device d, unsigned *n) { *n=150000; return 0; }
int nvmlDeviceGetMemoryInfo(Device d, Memory *m) { m->total=8192; m->free=6144; m->used=2048; return 0; }
int nvmlDeviceGetUtilizationRates(Device d, Util *u) { u->gpu=45; u->memory=20; return 0; }
int nvmlDeviceGetEncoderUtilization(Device d, unsigned *n, unsigned *period) { return 3; }
int nvmlDeviceGetDecoderUtilization(Device d, unsigned *n, unsigned *period) { *n=60; *period=100000; return 0; }
int nvmlDeviceGetTemperature(Device d, unsigned sensor, unsigned *n) { if(sensor)return 3; *n=43; return 0; }
int nvmlDeviceGetTemperatureThreshold(Device d, unsigned kind, unsigned *n) { *n=83; return 0; }
int nvmlDeviceGetClockInfo(Device d, unsigned kind, unsigned *n) { *n=kind==0?1234:5000; return 0; }
int nvmlDeviceGetFieldValues(Device d, int count, Field *f) { if(count!=1||f->id!=82)return 2; f->type=1; f->status=0; f->value=70; return 0; }
int nvmlDeviceGetCurrentClocksEventReasons(Device d, uint64_t *n) { *n=36; return 0; }
int nvmlDeviceGetSupportedClocksEventReasons(Device d, uint64_t *n) { *n=255; return 0; }
int nvmlDeviceGetNumFans(Device d, unsigned *n) { *n=2; return 0; }
int nvmlDeviceGetFanSpeed_v2(Device d, unsigned fan, unsigned *n) { if(fan>1)return 2; *n=fan?50:0; return 0; }
int nvmlDeviceGetFanSpeedRPM(Device d, Fan *f) { if(f->version!=(12|(1u<<24)))return 25; f->speed=f->fan?1000:0; return 0; }
